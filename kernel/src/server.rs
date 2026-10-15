//! stdio 上的换行分隔 JSON-RPC 2.0 服务（对应 dsh-sdk-jsonrpc-server + dsh-sdk-protocol）。
//! stdout 只输出 JSON-RPC 帧；日志一律走 stderr。

use crate::agent::{followup, AgentManager};
use crate::cordis::KernelEvent;
use crate::llm::{self, ContentBlock};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub const SERVER_NAME: &str = "deepseek-harness-sdk-runtime";
pub const SERVER_VERSION: &str = "0.0.1";

pub struct ServerState {
    pub initialized: bool,
    pub cwd: String,
    pub provider: String,
    pub model: String,
    pub max_tokens: Option<u64>,
}

pub async fn serve_stdio(agents: Arc<AgentManager>, bus: crate::cordis::EventBus) -> i32 {
    let state = Arc::new(tokio::sync::Mutex::new(ServerState {
        initialized: false,
        cwd: std::env::current_dir().unwrap_or_default().to_string_lossy().into(),
        provider: String::new(),
        model: String::new(),
        max_tokens: None,
    }));
    let stdout = Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));

    // 通知转发
    {
        let stdout = stdout.clone();
        let mut rx = bus.subscribe();
        tokio::spawn(async move {
            loop {
                let Ok(ev) = rx.recv().await else { continue };
                let (method, params) = match ev {
                    KernelEvent::SessionEvent { session_id, event } => {
                        ("session.event", json!({"sessionId": session_id, "event": event}))
                    }
                    KernelEvent::AgentStatus { session_id, status } => {
                        ("session.status", json!({"sessionId": session_id, "status": status}))
                    }
                };
                let line = json!({"jsonrpc": "2.0", "method": method, "params": params});
                let mut out = stdout.lock().await;
                let _ = out.write_all(serde_json::to_string(&line).unwrap().as_bytes()).await;
                let _ = out.write_all(b"\n").await;
                let _ = out.flush().await;
            }
        });
    }

    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let mut shutdown_code: Option<i32> = None;
    let mut inflight: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(method) = req.get("method").and_then(Value::as_str).map(String::from) else { continue };
        let id = req.get("id").cloned();
        let params = req.get("params").cloned().unwrap_or(json!({}));

        if method == "shutdown" {
            // 响应后再 dispose、退出
            let mut out = stdout.lock().await;
            let resp = json!({"jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "result": {}});
            let _ = out.write_all(serde_json::to_string(&resp).unwrap().as_bytes()).await;
            let _ = out.write_all(b"\n").await;
            let _ = out.flush().await;
            drop(out);
            agents.dispose_all().await;
            shutdown_code = Some(0);
            break;
        }

        let agents = agents.clone();
        let state = state.clone();
        let stdout = stdout.clone();
        let Some(id) = id else { continue }; // 通知不处理
        inflight.push(tokio::spawn(async move {
            let result = dispatch(&agents, &state, &method, params).await;
            let resp = match result {
                Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
                Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32603, "message": e}}),
            };
            let mut out = stdout.lock().await;
            let _ = out.write_all(serde_json::to_string(&resp).unwrap().as_bytes()).await;
            let _ = out.write_all(b"\n").await;
            let _ = out.flush().await;
        }));
        inflight.retain(|h| !h.is_finished());
    }
    // stdin 结束：等已接收的请求处理完再 dispose（对应 exitOnStdinEnd）
    for h in inflight {
        let _ = h.await;
    }
    if shutdown_code.is_none() {
        agents.dispose_all().await;
    }
    shutdown_code.unwrap_or(0)
}

async fn dispatch(
    agents: &Arc<AgentManager>,
    state: &Arc<tokio::sync::Mutex<ServerState>>,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    match method {
        "initialize" => {
            let mut st = state.lock().await;
            let cwd = params.get("cwd").and_then(Value::as_str).unwrap_or("");
            let provider = params.get("provider").and_then(Value::as_str).unwrap_or("");
            let model = params.get("model").and_then(Value::as_str).unwrap_or("");
            if provider.is_empty() || model.is_empty() {
                return Err("initialize requires provider and model".into());
            }
            if !agents.llm_providers.iter().any(|p| p.name == provider) {
                return Err(format!("no adapter registered for provider \"{provider}\""));
            }
            let cwd_abs = std::path::Path::new(if cwd.is_empty() { "." } else { cwd })
                .canonicalize()
                .map_err(|e| format!("initialize: bad cwd: {e}"))?
                .to_string_lossy()
                .to_string();
            if let Some(mt) = params.get("maxTokens") {
                let mt = mt.as_u64().filter(|v| *v > 0).ok_or("initialize maxTokens must be a positive safe integer")?;
                st.max_tokens = Some(mt);
            }
            st.cwd = cwd_abs;
            st.provider = provider.into();
            st.model = model.into();
            st.initialized = true;
            Ok(json!({"serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION}}))
        }
        "session/prompt" => {
            let st = state.lock().await;
            if !st.initialized {
                return Err("SDK server is not initialized".into());
            }
            let sid = params.get("sessionId").and_then(Value::as_str).unwrap_or("");
            let blocks: Vec<ContentBlock> = params
                .get("contentBlocks")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|b| {
                            if b.get("type")?.as_str()? == "text" {
                                Some(ContentBlock::Text {
                                    text: b.get("text")?.as_str()?.to_string(),
                                })
                            } else {
                                None
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            let agent = agents.get_or_create(sid, &st.cwd, &st.provider, &st.model, st.max_tokens);
            drop(st);
            let msg = llm::Message {
                id: llm::new_id(),
                role: "user".into(),
                content: blocks,
                source: json!({"kind": "user"}),
                tool_call_id: None,
                is_error: None,
            };
            let message_id = msg.id.clone();
            let _ = agent;
            followup(agents, sid, msg);
            Ok(json!({"messageId": message_id}))
        }
        "session/wait" => {
            let st = state.lock().await;
            if !st.initialized {
                return Err("SDK server is not initialized".into());
            }
            drop(st);
            let sid = params.get("sessionId").and_then(Value::as_str).unwrap_or("");
            let agent = agents.get(sid).ok_or_else(|| format!("unknown SDK session: {sid}"))?;
            let mut rx = agent.status.subscribe();
            loop {
                let idle = *rx.borrow() == "idle" && agent.inbox.lock().unwrap().is_empty();
                if idle {
                    // 稳定一拍再返回
                    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                    if *rx.borrow() == "idle" && agent.inbox.lock().unwrap().is_empty() {
                        return Ok(json!({}));
                    }
                }
                rx.changed().await.map_err(|_| "agent disposed".to_string())?;
            }
        }
        "session/working-directory/get" => {
            let st = state.lock().await;
            if !st.initialized {
                return Err("SDK server is not initialized".into());
            }
            let sid = params.get("sessionId").and_then(Value::as_str).unwrap_or("");
            let agent = agents.get_or_create(sid, &st.cwd, &st.provider, &st.model, st.max_tokens);
            drop(st);
            let cwd = agents.workdir.ensure(&agent.session.lock().unwrap())
                .map_err(|e| e)?.0;
            Ok(json!({"cwd": cwd}))
        }
        "session/working-directory/set" => {
            let st = state.lock().await;
            if !st.initialized {
                return Err("SDK server is not initialized".into());
            }
            let sid = params.get("sessionId").and_then(Value::as_str).unwrap_or("");
            let path = params.get("path").and_then(Value::as_str).unwrap_or("");
            let agent = agents.get_or_create(sid, &st.cwd, &st.provider, &st.model, st.max_tokens);
            drop(st);
            let (cwd, record) = agents.workdir.set(&agent.session.lock().unwrap(), path)?;
            if let Some(d) = record {
                agents.append(sid, "working-directory/change", json!({"cwd": d}), false);
            }
            Ok(json!({"cwd": cwd}))
        }
        _ => Err(format!("unknown method: {method}")),
    }
}
