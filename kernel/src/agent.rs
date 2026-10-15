//! agent 与 agent-loop（对应 dsh-agent + dsh-agent-loop）：
//! 调模型 → 执行工具 → 重复；取消保留已流出文本；maxParallelToolCalls 默认 10。

use crate::bash_tool::{BashConfig, BashService};
use crate::cordis::KernelEvent;
use crate::llm::{self, LlmFailure, Message, RetryPolicy, StreamCtx, StreamOutcome};
use crate::session::Session;
use crate::workdir::WorkingDirectory;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub const DEFAULT_MAX_PARALLEL_TOOL_CALLS: usize = 10;

pub struct AgentState {
    pub session: Arc<Mutex<Session>>,
    pub status: tokio::sync::watch::Sender<String>, // "idle" | "running"
    pub inbox: Arc<Mutex<VecDeque<Message>>>,
    pub cancel: Arc<AtomicBool>,
    pub wake: Arc<tokio::sync::Notify>,
    pub provider: String,
    pub model: String,
    pub max_tokens: Option<u64>,
    /// 本 loop 实例是否已记过 request/header（重启后新建实例为 false → 首个请求记 resume）
    pub header_logged: AtomicBool,
    /// 中止原因：user | disposed
    pub cancel_kind: Mutex<&'static str>,
}

/// agents 服务
pub struct AgentManager {
    pub agents: Mutex<HashMap<String, Arc<AgentState>>>,
    pub bus: crate::cordis::EventBus,
    pub http: reqwest::Client,
    pub workdir: Arc<WorkingDirectory>,
    pub bash: Arc<BashService>,
    pub llm_providers: Vec<llm::ProviderCfg>,
    pub retry: Option<RetryPolicy>, // llm-retry 插件存在时启用
    pub bash_config: BashConfig,
    pub tools: Arc<crate::tools::ToolsRegistry>, // 每步动态读取（插件注册的工具在此）
    pub max_parallel_tool_calls: usize,
}

impl AgentManager {
    /// 追加事件 = 写日志 + 持久化 + 发总线
    pub fn append(&self, session_id: &str, kind: &str, data: Value, surface_append: bool) {
        let op = if surface_append { Some(json!("append")) } else { None };
        self.append_full(session_id, kind, data, op, None);
    }

    pub fn append_full(&self, session_id: &str, kind: &str, data: Value, surface_op: Option<Value>, srcs: Option<Vec<u64>>) {
        let Some(agent) = self.get(session_id) else { return };
        let ev = agent.session.lock().unwrap().append_full(kind, data, surface_op, srcs);
        if let Err(e) = crate::persist::persist_event(session_id, &ev) {
            eprintln!("[persist] 写盘失败 {session_id} {kind}: {e}");
        }
        self.bus.emit(KernelEvent::SessionEvent { session_id: session_id.into(), event: ev });
    }

    pub fn set_status(&self, session_id: &str, status: &str) {
        let Some(agent) = self.get(session_id) else { return };
        if *agent.status.borrow() != status {
            agent.status.send_replace(status.into()); // send() 在无接收者时不存储值，必须用 send_replace
            self.bus.emit(KernelEvent::AgentStatus { session_id: session_id.into(), status: status.into() });
        }
    }

    pub fn get(&self, session_id: &str) -> Option<Arc<AgentState>> {
        self.agents.lock().unwrap().get(session_id).cloned()
    }

    /// 取或建 agent（恢复持久化日志）；sessionId 未知时新建
    pub fn get_or_create(
        self: &Arc<Self>,
        session_id: &str,
        cwd: &str,
        provider: &str,
        model: &str,
        max_tokens: Option<u64>,
    ) -> Arc<AgentState> {
        if let Some(a) = self.get(session_id) {
            return a;
        }
        let session = match crate::persist::load(session_id) {
            Some(s) => Arc::new(Mutex::new(s)),
            None => {
                let s = Session::new(session_id, Some(cwd.into()));
                crate::persist::persist_header(&s).ok();
                Arc::new(Mutex::new(s))
            }
        };
        let (status_tx, _) = tokio::sync::watch::channel("idle".to_string());
        let agent = Arc::new(AgentState {
            session,
            status: status_tx,
            inbox: Arc::new(Mutex::new(VecDeque::new())),
            cancel: Arc::new(AtomicBool::new(false)),
            wake: Arc::new(tokio::sync::Notify::new()),
            provider: provider.into(),
            model: model.into(),
            max_tokens,
            header_logged: AtomicBool::new(false),
            cancel_kind: Mutex::new("user"),
        });
        self.agents.lock().unwrap().insert(session_id.into(), agent.clone());
        let mgr = self.clone();
        let sid = session_id.to_string();
        let a = agent.clone();
        tokio::spawn(async move { agent_driver(mgr, sid, a).await });
        agent
    }

    pub async fn dispose_all(&self) {
        let all: Vec<Arc<AgentState>> = self.agents.lock().unwrap().values().cloned().collect();
        for a in &all {
            *a.cancel_kind.lock().unwrap() = "disposed";
            a.cancel.store(true, Ordering::Relaxed);
            a.wake.notify_one();
        }
        // 等驱动退出（中止结算会写日志）
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let running = all.iter().any(|a| *a.status.borrow() == "running");
            if !running || std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        self.bash.kill_all();
    }
}

/// prompt 入队并唤醒驱动
pub fn followup(mgr: &AgentManager, session_id: &str, msg: Message) {
    let Some(agent) = mgr.get(session_id) else { return };
    agent.inbox.lock().unwrap().push_back(msg);
    agent.wake.notify_one();
}

async fn agent_driver(mgr: Arc<AgentManager>, session_id: String, agent: Arc<AgentState>) {
    loop {
        if agent.inbox.lock().unwrap().is_empty() {
            agent.wake.notified().await;
            continue;
        }
        if agent.cancel.load(Ordering::Relaxed) {
            continue; // dispose 后不起新 turn
        }
        mgr.set_status(&session_id, "running");
        let reason = run_turn(&mgr, &session_id, &agent).await;
        let turn = current_turn(&agent);
        mgr.append(&session_id, "turn/end", json!({"turn": turn, "reason": reason}), false);
        mgr.set_status(&session_id, "idle");
    }
}

fn current_turn(agent: &AgentState) -> u64 {
    agent
        .session
        .lock()
        .unwrap()
        .events
        .iter()
        .rev()
        .find(|e| e.kind == "turn/start")
        .and_then(|e| e.data.get("turn").and_then(Value::as_u64))
        .unwrap_or(0)
}

async fn run_turn(mgr: &Arc<AgentManager>, sid: &str, agent: &Arc<AgentState>) -> Value {
    let turn = current_turn(agent) + 1;
    mgr.append(sid, "turn/start", json!({"turn": turn}), false);

    let mut step = 0u64;
    let mut reason = json!({"kind": "completed"});
    // 第一步认领 prompt；后续步骤认领注入的 notice（如工作目录变化）
    let mut messages: Vec<Message> = agent.inbox.lock().unwrap().drain(..).collect();
    if messages.is_empty() {
        return reason;
    }
    loop {
        step += 1;
        mgr.append(sid, "step/start", json!({"turn": turn, "step": step}), false);
        let outcome = run_step(mgr, sid, agent, turn, step, messages).await;
        mgr.append(sid, "step/end", json!({"turn": turn, "step": step}), false);
        match outcome {
            StepOutcome::Done(r) => {
                reason = r;
                break;
            }
            StepOutcome::Continue => {
                if agent.cancel.load(Ordering::Relaxed) {
                    reason = abort_reason(agent);
                    break;
                }
                messages = agent.inbox.lock().unwrap().drain(..).collect();
            }
        }
    }
    reason
}

enum StepOutcome {
    Continue,
    Done(Value),
}

async fn run_step(
    mgr: &Arc<AgentManager>,
    sid: &str,
    agent: &Arc<AgentState>,
    turn: u64,
    step: u64,
    messages: Vec<Message>,
) -> StepOutcome {
    // 1. 工作目录 ensure（目录消失时恢复原始目录）
    let cwd = {
        let session = agent.session.lock().unwrap();
        match mgr.workdir.ensure(&session) {
            Ok((cwd, record)) => {
                drop(session);
                if let Some(c) = record {
                    mgr.append(sid, "working-directory/change", json!({"cwd": c}), false);
                }
                cwd
            }
            Err(e) => {
                return StepOutcome::Done(json!({"kind": "error", "error": {"message": e, "code": "UNKNOWN"}}))
            }
        }
    };

    // 2. 认领的 user 消息上 surface
    for m in &messages {
        mgr.append(sid, "user/message", serde_json::to_value(m).unwrap(), true);
    }

    // 3. runtime context 快照（变化时追加；对应 RuntimeContextProjection）
    let snapshot = crate::sysprompt::context_snapshot(&[crate::workdir::context_text(&cwd)]);
    let retained = last_runtime_context(&agent.session.lock().unwrap());
    let snapshot_msg = if !snapshot.is_empty() && retained.as_deref() != Some(snapshot.as_str()) {
        Some(snapshot)
    } else if snapshot.is_empty() && retained.is_some() {
        Some(crate::sysprompt::CONTEXT_CLEARED.to_string())
    } else {
        None
    };
    if let Some(text) = snapshot_msg {
        let msg = llm::create_user_message(
            &text,
            json!({"kind": "runtime-context", "form": "snapshot", "sections": [
                {"name": "working-directory:current", "text": crate::workdir::context_text(&cwd)}
            ]}),
        );
        mgr.append(sid, "user/message", serde_json::to_value(msg).unwrap(), true);
    }

    // 4. system/message：surface 节点 0（只写一次）
    {
        let has_system = agent.session.lock().unwrap().events.iter().any(|e| e.kind == "system/message");
        if !has_system {
            let msg = llm::create_system_message(&crate::sysprompt::persona());
            mgr.append(
                sid,
                "system/message",
                json!({"turn": turn, "step": step, "message": serde_json::to_value(msg).unwrap()}),
                true,
            );
        }
    }

    // 5. request/header + request/context
    let tools = tool_schemas(mgr);
    log_request_header(mgr, sid, agent, &tools);

    // 6. 流式调用（step 边界重试）
    let provider = match mgr.llm_providers.iter().find(|p| p.name == agent.provider) {
        Some(p) => p.clone(),
        None => {
            return StepOutcome::Done(json!({
                "kind": "error",
                "error": {"message": format!("no adapter registered for provider {:?}", agent.provider), "code": "NO_ADAPTER"}
            }));
        }
    };
    let wire_tools = llm::to_wire_tools(&tools);
    let policy = provider.retry_policy.clone().or(mgr.retry.clone());

    let mut attempt = 0u32;
    let outcome: StreamOutcome = loop {
        let wire_messages = llm::to_wire_messages(&agent.session.lock().unwrap().derive_messages());
        let res = llm::stream_chat(
            &mgr.http,
            &StreamCtx { provider: provider.clone(), model: agent.model.clone(), max_tokens: agent.max_tokens },
            wire_messages,
            wire_tools.clone(),
            agent.cancel.clone(),
            |_delta| {},
        )
        .await;
        match res {
            Ok(o) if !o.aborted => break o,
            Ok(o) => {
                settle_interrupted(mgr, sid, turn, step, &o, &agent.provider, &agent.model);
                return StepOutcome::Done(abort_reason(agent));
            }
            Err(failure) => {
                mgr.append(sid, "assistant/attempt", json!({"turn": turn, "step": step, "stream": []}), false);
                let max = policy.as_ref().map(|p| p.max_retries).unwrap_or(0);
                if policy.as_ref().map(|p| !failure_retryable(&failure, Some(p))).unwrap_or(true)
                    || attempt >= max
                    || agent.cancel.load(Ordering::Relaxed)
                {
                    if agent.cancel.load(Ordering::Relaxed) {
                        return StepOutcome::Done(abort_reason(agent));
                    }
                    return StepOutcome::Done(json!({"kind": "error", "error": failure_json(&failure)}));
                }
                attempt += 1;
                let p = policy.as_ref().unwrap();
                let delay = p.delay_ms(attempt, rand01());
                let retry_id = llm::new_id();
                let policy_key = json!([
                    "normal", p.max_retries, sorted_codes(p), p.initial_delay_ms, p.max_delay_ms, p.jitter_ratio
                ])
                .to_string();
                // 重试记录先写入会话日志再退避
                mgr.append(
                    sid,
                    "llm/retry",
                    json!({
                        "retryId": retry_id, "turn": turn, "step": step,
                        "provider": provider.name, "mode": "normal", "policyKey": policy_key,
                        "retry": attempt, "maxRetries": p.max_retries, "delayMs": delay,
                        "failure": failure_json(&failure),
                    }),
                    false,
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                if agent.cancel.load(Ordering::Relaxed) {
                    return StepOutcome::Done(abort_reason(agent));
                }
                mgr.append(
                    sid,
                    "llm/retry-started",
                    json!({"retryId": retry_id, "turn": turn, "step": step, "retry": attempt}),
                    false,
                );
            }
        }
    };

    // 7. assistant/message
    let msg = llm::create_assistant_message(outcome.blocks(), &agent.provider, &agent.model);
    {
        let mut data = json!({
            "turn": turn, "step": step,
            "message": serde_json::to_value(&msg).unwrap(),
            "stream": outcome.stream,
        });
        if let Some(u) = &outcome.usage {
            data["usage"] = serde_json::to_value(u).unwrap();
        }
        mgr.append(sid, "assistant/message", data, true);
    }

    if outcome.finish == llm::FinishKind::MaxTokens {
        return StepOutcome::Done(json!({"kind": "max-tokens"}));
    }

    // 8. 工具调用
    let tool_calls: Vec<(String, String, String)> = outcome
        .tool_calls
        .iter()
        .filter(|(id, _, _)| !id.is_empty())
        .cloned()
        .collect();
    if tool_calls.is_empty() {
        return StepOutcome::Done(json!({"kind": "completed"}));
    }
    let aborted = execute_tool_calls(mgr, sid, agent, turn, step, &tool_calls, &cwd).await;
    if aborted {
        return StepOutcome::Done(abort_reason(agent));
    }
    StepOutcome::Continue
}

/// 并行执行（上限 maxParallelToolCalls），结果按模型顺序提交
async fn execute_tool_calls(
    mgr: &Arc<AgentManager>,
    sid: &str,
    agent: &Arc<AgentState>,
    turn: u64,
    step: u64,
    calls: &[(String, String, String)],
    cwd: &str,
) -> bool {
    let mut call_seqs = Vec::new();
    for (id, name, args) in calls {
        let e = {
            let mut s = agent.session.lock().unwrap();
            s.append("tool/call", json!({"turn": turn, "step": step, "callId": id, "name": name, "arguments": args}), false)
        };
        crate::persist::persist_event(sid, &e).ok();
        mgr.bus.emit(KernelEvent::SessionEvent { session_id: sid.into(), event: e.clone() });
        call_seqs.push(e.seq);
    }

    let sem = Arc::new(tokio::sync::Semaphore::new(mgr.max_parallel_tool_calls));
    let mut handles = Vec::new();
    for (idx, (_id, name, args)) in calls.iter().cloned().enumerate() {
        let mgr = mgr.clone();
        let sid = sid.to_string();
        let cwd = cwd.to_string();
        let cancel = agent.cancel.clone();
        let sem = sem.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            if cancel.load(Ordering::Relaxed) {
                return (idx, None);
            }
            (idx, Some(execute_one(&mgr, &sid, &name, &args, &cwd, cancel.clone()).await))
        }));
    }
    let mut results: Vec<Option<(String, bool)>> = vec![None; calls.len()];
    let mut aborted = false;
    for h in handles {
        match h.await {
            Ok((idx, Some(r))) => results[idx] = Some(r),
            Ok((idx, None)) => {
                results[idx] = Some(("Error: tool call aborted before dispatch".into(), true));
                aborted = true;
            }
            Err(_) => {
                aborted = true;
            }
        }
    }
    for (idx, (id, _, _)) in calls.iter().enumerate() {
        let (text, is_error) = results[idx].take().unwrap();
        let msg = llm::create_tool_result_message(id, &text, is_error);
        let mut data = json!({"turn": turn, "step": step, "message": serde_json::to_value(msg).unwrap()});
        if is_error && text.contains("aborted before dispatch") {
            data["error"] = json!({"name": "AbortError", "code": "TOOL_ABORTED_BEFORE_DISPATCH"});
        }
        mgr.append_full(sid, "tool/result", data, Some(json!("append")), Some(vec![call_seqs[idx]]));
    }
    aborted
}

async fn execute_one(
    mgr: &AgentManager,
    sid: &str,
    name: &str,
    args_raw: &str,
    cwd: &str,
    cancel: Arc<AtomicBool>,
) -> (String, bool) {
    let args: Value = serde_json::from_str(args_raw).unwrap_or(json!({}));
    match name {
        "bash" => {
            let cmd = args.get("command").and_then(Value::as_str).unwrap_or("");
            match mgr.bash.execute(sid, cmd, cwd, mgr.bash_config, cancel).await {
                Ok(text) => (text, false),
                Err(e) => (format!("Error: {e}"), true),
            }
        }
        "working_directory" if mgr.tools.has("working_directory") => {
            let Some(agent) = mgr.get(sid) else {
                return ("Error: working_directory requires an agent Session".into(), true);
            };
            let result = {
                let session = agent.session.lock().unwrap();
                match args.get("cd").and_then(Value::as_str) {
                    None => mgr.workdir.ensure(&session),
                    Some(path) => mgr.workdir.set(&session, path),
                }
            };
            match result {
                Ok((dir, record)) => {
                    if let Some(d) = record {
                        let prev = mgr.workdir.get(&agent.session.lock().unwrap());
                        mgr.append(sid, "working-directory/change", json!({"cwd": d}), false);
                        if prev != d {
                            let notice = llm::create_user_message(
                                &format!(
                                    "The working directory changed from {} to {}.",
                                    serde_json::to_string(&prev).unwrap(),
                                    serde_json::to_string(&d).unwrap()
                                ),
                                json!({"kind": "working-directory"}),
                            );
                            followup(mgr, sid, notice);
                        }
                    }
                    (dir, false)
                }
                Err(e) => (format!("Error: {e}"), true),
            }
        }
        _ => (format!("Error: unknown tool: {name}"), true),
    }
}

/// 取消时保留已流出文本（interrupted: true）
fn settle_interrupted(mgr: &Arc<AgentManager>, sid: &str, turn: u64, step: u64, o: &StreamOutcome, provider: &str, model: &str) {
    let blocks = o.blocks();
    if blocks.is_empty() {
        mgr.append(sid, "assistant/attempt", json!({"turn": turn, "step": step, "stream": o.stream}), false);
        return;
    }
    let msg = llm::create_assistant_message(blocks, provider, model);
    let mut data = json!({
        "turn": turn, "step": step,
        "message": serde_json::to_value(&msg).unwrap(),
        "interrupted": true,
        "stream": o.stream,
    });
    if let Some(u) = &o.usage {
        data["usage"] = serde_json::to_value(u).unwrap();
    }
    mgr.append(sid, "assistant/message", data, true);
}

fn last_runtime_context(session: &Session) -> Option<String> {
    session.events.iter().rev().find_map(|e| {
        if e.kind != "user/message" {
            return None;
        }
        if e.data.get("source")?.get("kind")?.as_str()? != "runtime-context" {
            return None;
        }
        e.data.get("content")?.as_array()?.first()?.get("text")?.as_str().map(String::from)
    })
}

fn tool_schemas(mgr: &AgentManager) -> Vec<llm::ToolSchema> {
    mgr.tools.list()
}

/// request/header：首个请求记 initial（日志已有 header 则记 resume），变化记 change
fn log_request_header(mgr: &Arc<AgentManager>, sid: &str, agent: &Arc<AgentState>, tools: &[llm::ToolSchema]) {
    let mut config = json!({"provider": agent.provider, "model": agent.model});
    if let Some(m) = agent.max_tokens {
        config["maxTokens"] = json!(m);
    }
    let new_header = json!({"config": config, "tools": tools});
    let baseline = agent.session.lock().unwrap().request_header().cloned();
    let reason = if !agent.header_logged.swap(true, Ordering::Relaxed) {
        // 本 loop 实例首个请求
        Some(if baseline.is_some() { "resume" } else { "initial" })
    } else if baseline.and_then(|b| b.get("header").cloned()).as_ref() != Some(&new_header) {
        Some("change")
    } else {
        None
    };
    if let Some(reason) = reason {
        mgr.append(sid, "request/header", json!({"header": new_header, "reason": reason}), false);
    }
    let ctx = json!({"provider": agent.provider, "model": agent.model});
    let changed = {
        let s = agent.session.lock().unwrap();
        s.request_context()
            .map(|p| p.get("provider") != ctx.get("provider") || p.get("model") != ctx.get("model"))
            .unwrap_or(true)
    };
    if changed {
        mgr.append(sid, "request/context", ctx, false);
    }
}

fn failure_retryable(f: &LlmFailure, policy: Option<&RetryPolicy>) -> bool {
    match policy {
        Some(p) => p.retryable_codes.iter().any(|c| c == &f.code),
        None => false,
    }
}

fn failure_json(f: &LlmFailure) -> Value {
    serde_json::to_value(f).unwrap()
}

fn sorted_codes(p: &RetryPolicy) -> Vec<String> {
    let mut c = p.retryable_codes.clone();
    c.sort();
    c
}


fn abort_reason(agent: &AgentState) -> Value {
    json!({"kind": "aborted", "reason": {"kind": *agent.cancel_kind.lock().unwrap()}})
}

fn rand01() -> f64 {
    uuid::Uuid::new_v4().as_bytes()[0] as f64 / 255.0
}
