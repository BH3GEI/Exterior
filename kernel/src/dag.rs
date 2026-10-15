//! Exterior 专用插件：节点 DAG 替代内核默认的线性会话投影（SPEC §3）。
//! 一个节点就是一个念头；没有 session/会话/项目。存储在 ~/.exterior/nodes.jsonl。

use crate::llm::{ContentBlock, Message};
use crate::projection::SessionProjection;
use crate::session::{Event, Session};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::Mutex;

// ---------- 节点（SPEC §2.1） ----------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub cmd: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DagNode {
    pub id: String,
    pub parents: Vec<String>,
    pub kind: String, // user | assistant | bash
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<ToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    pub created_at: String,
}

impl DagNode {
    pub fn new(parents: Vec<String>, kind: &str) -> Self {
        DagNode {
            id: ulid::Ulid::new().to_string(),
            parents,
            kind: kind.into(),
            text: None,
            model: None,
            tool_call: None,
            cmd: None,
            output: None,
            exit_code: None,
            call_id: None,
            created_at: now_rfc3339(),
        }
    }
}

fn now_rfc3339() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    // 简化 RFC3339：epoch ms → UTC
    let secs = (ms / 1000) as i64;
    let days = secs / 86400;
    let rem = secs % 86400;
    let (y, mo, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y, mo, d, rem / 3600, (rem % 3600) / 60, rem % 60, ms % 1000
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------- 存储（SPEC §2.2） ----------

pub fn exterior_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".exterior")
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct View {
    #[serde(default)]
    pub collapsed: Vec<String>,
    #[serde(default)]
    pub last_model: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExteriorModelCfg {
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key_env: Option<String>,
    pub model: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExteriorConfig {
    #[serde(default)]
    pub models: Vec<ExteriorModelCfg>,
    #[serde(default)]
    pub default: Option<String>,
}

pub struct DagStore {
    pub nodes: HashMap<String, DagNode>,
    pub children: HashMap<String, Vec<String>>,
    pub order: Vec<String>,
    pub view: Mutex<View>,
}

impl DagStore {
    pub fn load() -> DagStore {
        let mut s = DagStore {
            nodes: HashMap::new(),
            children: HashMap::new(),
            order: Vec::new(),
            view: Mutex::new(read_view()),
        };
        let path = exterior_dir().join("nodes.jsonl");
        if path.exists() {
            if let Ok(f) = fs::File::open(path) {
                for line in BufReader::new(f).lines().map_while(Result::ok) {
                    if line.trim().is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<DagNode>(&line) {
                        Ok(n) => s.insert(n),
                        Err(e) => eprintln!("[dag] 跳过坏行：{e}"),
                    }
                }
            }
        }
        s
    }

    pub fn insert(&mut self, n: DagNode) {
        for p in &n.parents {
            self.children.entry(p.clone()).or_default().push(n.id.clone());
        }
        self.order.push(n.id.clone());
        self.nodes.insert(n.id.clone(), n);
    }

    /// 追加写盘（单次 write_all）+ 内存索引
    pub fn append(&mut self, n: DagNode) -> std::io::Result<()> {
        fs::create_dir_all(exterior_dir())?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(exterior_dir().join("nodes.jsonl"))?;
        let mut s = serde_json::to_string(&n).unwrap();
        s.push('\n');
        f.write_all(s.as_bytes())?;
        self.insert(n);
        Ok(())
    }

    pub fn save_view(&self) -> std::io::Result<()> {
        fs::create_dir_all(exterior_dir())?;
        fs::write(
            exterior_dir().join("view.json"),
            serde_json::to_string_pretty(&*self.view.lock().unwrap())?,
        )
    }
}

fn read_view() -> View {
    fs::read_to_string(exterior_dir().join("view.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn read_exterior_config() -> Result<ExteriorConfig, String> {
    let path = exterior_dir().join("config.json");
    let s = fs::read_to_string(&path)
        .map_err(|_| format!("config.json 不存在：{}（参考 SPEC §2.3 创建）", path.display()))?;
    serde_json::from_str(&s).map_err(|e| format!("config.json 解析失败：{e}"))
}

// ---------- 上下文组装（SPEC §3.1） ----------

const MAX_BASH_OUTPUT: usize = 16_000;

fn subtree(store: &DagStore, id: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    let mut stack = vec![id.to_string()];
    while let Some(cur) = stack.pop() {
        if set.insert(cur.clone()) {
            if let Some(n) = store.nodes.get(&cur) {
                stack.extend(n.parents.iter().cloned());
            }
        }
    }
    set
}

fn sort_ids(store: &DagStore, ids: &mut Vec<String>) {
    ids.sort(); // ULID 按时间可排序，父一定先于子
    ids.retain(|id| store.nodes.contains_key(id));
}

/// 先共同部分，再按 parents 顺序逐条分支；合并处分支开头插 [来路 i/n]
pub fn context_order(store: &DagStore, target: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    walk(store, target, &mut visited, &mut out);
    out
}

fn walk(store: &DagStore, id: &str, visited: &mut HashSet<String>, out: &mut Vec<(String, Option<String>)>) {
    let Some(node) = store.nodes.get(id) else { return };
    if node.parents.len() <= 1 {
        for p in &node.parents {
            if !visited.contains(p) {
                walk(store, p, visited, out);
            }
        }
    } else {
        let sets: Vec<HashSet<String>> = node.parents.iter().map(|p| subtree(store, p)).collect();
        let mut common = sets[0].clone();
        for s in &sets[1..] {
            common = common.intersection(s).cloned().collect();
        }
        let mut cv: Vec<String> = common.into_iter().collect();
        sort_ids(store, &mut cv);
        for c in cv {
            if visited.insert(c.clone()) {
                out.push((c, None));
            }
        }
        let n = node.parents.len();
        for i in 0..n {
            let mut bv: Vec<String> = sets[i].iter().filter(|x| !visited.contains(*x)).cloned().collect();
            sort_ids(store, &mut bv);
            if bv.is_empty() {
                continue;
            }
            for (j, b) in bv.into_iter().enumerate() {
                visited.insert(b.clone());
                let marker = (j == 0).then(|| format!("[来路 {}/{}]", i + 1, n));
                out.push((b, marker));
            }
        }
    }
    if visited.insert(id.to_string()) {
        out.push((id.to_string(), None));
    }
}

fn truncate_output(s: &str) -> String {
    if s.len() <= MAX_BASH_OUTPUT {
        return s.to_string();
    }
    let half = MAX_BASH_OUTPUT / 2;
    let head_end = floor_boundary(s, half);
    let tail_start = floor_boundary(s, s.len().saturating_sub(half));
    format!(
        "{}\n[...省略 {} 字符...]\n{}",
        &s[..head_end],
        s.len() - MAX_BASH_OUTPUT,
        &s[tail_start..]
    )
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// DAG → 消息序列（不含系统提示，系统提示由调用方加）
pub fn dag_messages(store: &DagStore, target: &str) -> Vec<Message> {
    let mut out = Vec::new();
    for (id, marker) in context_order(store, target) {
        if let Some(m) = marker {
            out.push(crate::llm::create_user_message(&m, json!({"kind": "runtime-context"})));
        }
        let Some(node) = store.nodes.get(&id) else { continue };
        match node.kind.as_str() {
            "user" => out.push(crate::llm::create_user_message(
                node.text.as_deref().unwrap_or_default(),
                json!({"kind": "user"}),
            )),
            "assistant" => {
                let mut content: Vec<ContentBlock> = Vec::new();
                if let Some(t) = &node.text {
                    if !t.is_empty() {
                        content.push(ContentBlock::Text { text: t.clone() });
                    }
                }
                if let Some(tc) = &node.tool_call {
                    content.push(ContentBlock::ToolCall {
                        id: tc.id.clone(),
                        name: "bash".into(),
                        arguments: json!({"command": tc.cmd}).to_string(),
                    });
                    // 一次多个 tool call：后续的只有 bash 子节点留下 call_id
                    if let Some(kids) = store.children.get(&node.id) {
                        for kid in kids {
                            if let Some(k) = store.nodes.get(kid) {
                                if k.kind == "bash" {
                                    if let (Some(cid), Some(cmd)) = (&k.call_id, &k.cmd) {
                                        if *cid != tc.id {
                                            content.push(ContentBlock::ToolCall {
                                                id: cid.clone(),
                                                name: "bash".into(),
                                                arguments: json!({"command": cmd}).to_string(),
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                out.push(Message {
                    id: crate::llm::new_id(),
                    role: "assistant".into(),
                    content,
                    source: json!({"kind": "model", "provider": "exterior", "model": node.model.clone().unwrap_or_default()}),
                    tool_call_id: None,
                    is_error: None,
                });
            }
            "bash" => {
                out.push(Message {
                    id: crate::llm::new_id(),
                    role: "tool".into(),
                    content: vec![ContentBlock::Text {
                        text: truncate_output(node.output.as_deref().unwrap_or_default()),
                    }],
                    source: json!({"kind": "tool", "callId": node.call_id.clone().unwrap_or_default()}),
                    tool_call_id: node.call_id.clone(),
                    is_error: None,
                });
            }
            _ => {}
        }
    }
    out
}

// ---------- DAG 投影（替换内核的线性投影） ----------

pub struct DagProjection {
    pub store: Mutex<DagStore>,
    /// run(=user 节点 id) → 当前末端节点
    pub current: Mutex<HashMap<String, String>>,
    /// session_id → run id
    pub session_run: Mutex<HashMap<String, String>>,
    /// run id → 模型显示名（config 里的 name，SPEC §4.3）
    pub run_model: Mutex<HashMap<String, String>>,
    /// 节点事件通道（给 GUI）
    pub events: tokio::sync::broadcast::Sender<DagEvent>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DagEvent {
    Node { run: String, node: DagNode },
    Delta { run: String, text: String },
    Bash { run: String, chunk: String },
    RunEnd { run: String },
}

impl DagProjection {
    pub fn new() -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(4096);
        DagProjection {
            store: Mutex::new(DagStore::load()),
            current: Mutex::new(HashMap::new()),
            session_run: Mutex::new(HashMap::new()),
            run_model: Mutex::new(HashMap::new()),
            events: tx,
        }
    }

    pub fn store<R>(&self, f: impl FnOnce(&DagStore) -> R) -> R {
        f(&self.store.lock().unwrap())
    }

    pub fn store_mut<R>(&self, f: impl FnOnce(&mut DagStore) -> R) -> R {
        f(&mut self.store.lock().unwrap())
    }

    /// 一次思考的入口（SPEC §3.2）：写 user 节点，返回 run id
    pub fn think(&self, parents: Vec<String>, text: String) -> Result<DagNode, String> {
        let mut user = DagNode::new(parents, "user");
        user.text = Some(text);
        let run = user.id.clone();
        self.store_mut(|s| s.append(user.clone()).map_err(|e| e.to_string()))?;
        self.current.lock().unwrap().insert(run.clone(), run.clone());
        let _ = self.events.send(DagEvent::Node { run: run.clone(), node: user.clone() });
        Ok(user)
    }

    fn advance(&self, run: &str, node: DagNode) {
        self.current.lock().unwrap().insert(run.into(), node.id.clone());
        let _ = self.events.send(DagEvent::Node { run: run.into(), node });
    }
}

impl SessionProjection for DagProjection {
    /// 内核事件 → DAG 节点（同步，保证下一步 derive 时链是最新的）
    fn observe(&self, session_id: &str, _session: &Session, ev: &Event) {
        let run = match self.session_run.lock().unwrap().get(session_id) {
            Some(r) => r.clone(),
            None => return,
        };
        match ev.kind.as_str() {
            "assistant/message" => {
                let msg = &ev.data["message"];
                let mut n = DagNode::new(vec![self.current.lock().unwrap()[&run].clone()], "assistant");
                let text: String = msg["content"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|b| b["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default();
                if !text.is_empty() {
                    n.text = Some(text);
                }
                n.model = self.run_model.lock().unwrap().get(&run).cloned()
                    .or_else(|| msg["source"]["model"].as_str().map(String::from));
                // 第一个 tool-call 块 → tool_call
                if let Some(tc) = msg["content"].as_array().and_then(|a| {
                    a.iter().find(|b| b["type"].as_str() == Some("tool-call"))
                }) {
                    let args = tc["arguments"].as_str().unwrap_or("{}");
                    let cmd = serde_json::from_str::<Value>(args)
                        .ok()
                        .and_then(|v| v["command"].as_str().map(String::from))
                        .unwrap_or_default();
                    n.tool_call = Some(ToolCall {
                        id: tc["id"].as_str().unwrap_or_default().into(),
                        cmd,
                    });
                }
                self.store_mut(|s| s.append(n.clone()).ok());
                self.advance(&run, n);
            }
            "tool/result" => {
                // 与最近的 tool/call 配对
                let msg = &ev.data["message"];
                let call_id = msg["toolCallId"].as_str().unwrap_or_default();
                // 找对应的 tool/call 事件拿 cmd
                let mut cmd = String::new();
                // 从 session 事件里找（observe 拿不到 session 事件历史就退化）——用 session 参数
                let _ = &mut cmd;
                let output: String = msg["content"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join(""))
                    .unwrap_or_default();
                let mut n = DagNode::new(vec![self.current.lock().unwrap()[&run].clone()], "bash");
                n.cmd = Some(self.cmd_for_call(_session, call_id));
                n.output = Some(output);
                n.call_id = Some(call_id.into());
                n.exit_code = ev.data["meta"]["exitCode"].as_i64().map(|v| v as i32);
                self.store_mut(|s| s.append(n.clone()).ok());
                self.advance(&run, n);
            }
            "turn/end" => {
                let _ = self.events.send(DagEvent::RunEnd { run });
            }
            _ => {}
        }
    }

    fn derive(&self, session_id: &str, _session: &Session) -> Vec<Message> {
        let run = match self.session_run.lock().unwrap().get(session_id) {
            Some(r) => r.clone(),
            None => return vec![],
        };
        let current = match self.current.lock().unwrap().get(&run) {
            Some(c) => c.clone(),
            None => return vec![],
        };
        let mut msgs = vec![Message {
            id: crate::llm::new_id(),
            role: "system".into(),
            content: vec![ContentBlock::Text {
                text: "你是用户思考的一部分，不是助手。你可以用 bash 访问整台电脑。所有过去的念头都在 ~/.exterior/nodes.jsonl 里，需要时自己去找。".into(),
            }],
            source: json!({"kind": "system-prompt"}),
            tool_call_id: None,
            is_error: None,
        }];
        self.store(|s| msgs.extend(dag_messages(s, &current)));
        msgs
    }
}

/// 一次思考（SPEC §3.2）：写 user 节点 → 起/用 kernel 会话（run id = 节点 id）→ 驱动 agent loop
pub fn think_and_run(
    dag: &std::sync::Arc<DagProjection>,
    agents: &std::sync::Arc<crate::agent::AgentManager>,
    parents: Vec<String>,
    text: String,
    model: Option<String>,
) -> Result<String, String> {
    let cfg = read_exterior_config()?;
    let mcfg = match &model {
        Some(name) => cfg
            .models
            .iter()
            .find(|m| &m.name == name)
            .ok_or_else(|| format!("config.json 里没有模型 {name}"))?,
        None => {
            let name = cfg.default.as_deref().unwrap_or("");
            cfg.models
                .iter()
                .find(|m| m.name == name)
                .or(cfg.models.first())
                .ok_or("config.json 里没有可用模型")?
        }
    };
    let node = dag.think(parents, text.clone())?;
    let run = node.id.clone();
    dag.run_model.lock().unwrap().insert(run.clone(), mcfg.name.clone());
    dag.session_run.lock().unwrap().insert(run.clone(), run.clone());
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    agents.get_or_create(&run, &home, &mcfg.name, &mcfg.model, None);
    let msg = crate::llm::create_user_message(&text, json!({"kind": "user"}));
    crate::agent::followup(agents, &run, msg);
    Ok(run)
}

impl DagProjection {
    fn cmd_for_call(&self, session: &Session, call_id: &str) -> String {
        session
            .events
            .iter()
            .rev()
            .find(|e| e.kind == "tool/call" && e.data["callId"].as_str() == Some(call_id))
            .and_then(|e| {
                let args = e.data["arguments"].as_str().unwrap_or("{}");
                serde_json::from_str::<Value>(args)
                    .ok()
                    .and_then(|v| v["command"].as_str().map(String::from))
            })
            .unwrap_or_default()
    }
}
