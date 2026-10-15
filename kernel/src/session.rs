//! 会话事件日志：只追加，surface 推导模型可见历史（对应 dsh-session + dsh-session-projection 的核心）。

use crate::llm::Message;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SESSION_FORMAT_VERSION: u32 = 4;

/// session 服务（cordis 服务名 "session"）：持有可替换的持久化实现（后绑定，启动顺序无关）
#[derive(Default)]
pub struct SessionServices {
    pub persistence: std::sync::RwLock<Option<std::sync::Arc<dyn crate::persist::SessionPersistence>>>,
    /// 投影服务（sessionProjections）：默认线性，可替换（SPEC §3）
    pub projection: std::sync::RwLock<Option<std::sync::Arc<dyn crate::projection::SessionProjection>>>,
}

/// 产生模型消息、可上 surface 的事件类型
pub const SURFACE_TYPES: [&str; 5] = [
    "system/message",
    "developer/message",
    "user/message",
    "assistant/message",
    "tool/result",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    #[serde(rename = "type")]
    pub kind: String,
    pub seq: u64,
    pub time: i64,
    #[serde(default)]
    pub data: Value,
    #[serde(rename = "surfaceOp", skip_serializing_if = "Option::is_none")]
    pub surface_op: Option<Value>,
    #[serde(rename = "sourceEventSeqs", skip_serializing_if = "Option::is_none")]
    pub source_event_seqs: Option<Vec<u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignorable: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionHeader {
    pub version: u32,
    pub id: String,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
    pub is_seeded: bool,
    pub delegation_depth: u32,
}

pub struct Session {
    pub header: SessionHeader,
    pub events: Vec<Event>,
    /// 有序 surface：产生消息的事件 seq 列表
    pub surface: Vec<u64>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Session {
    pub fn new(id: &str, cwd: Option<String>) -> Session {
        Session {
            header: SessionHeader {
                version: SESSION_FORMAT_VERSION,
                id: id.into(),
                created_at: now_ms(),
                cwd,
                parent_session: None,
                is_seeded: false,
                delegation_depth: 0,
            },
            events: Vec::new(),
            surface: Vec::new(),
        }
    }

    /// 从持久化恢复（事件已按 seq 排列）
    pub fn restore(header: SessionHeader, events: Vec<Event>) -> Session {
        let mut s = Session { header, events: vec![], surface: vec![] };
        for e in events {
            s.apply_surface(&e);
            s.events.push(e);
        }
        s
    }

    pub fn seq(&self) -> u64 {
        self.events.len() as u64
    }

    fn apply_surface(&mut self, e: &Event) {
        if !SURFACE_TYPES.contains(&e.kind.as_str()) {
            return;
        }
        match &e.surface_op {
            Some(Value::String(s)) if s == "append" => self.surface.push(e.seq),
            Some(Value::Null) | None => {}
            Some(Value::Object(op)) if op.get("op").and_then(Value::as_str) == Some("replace") => {
                let start = op.get("startSeq").and_then(Value::as_u64).unwrap_or(0);
                let end = op.get("endSeq").and_then(Value::as_u64).unwrap_or(0);
                self.surface.retain(|seq| *seq < start || *seq > end);
                self.surface.push(e.seq);
            }
            _ => {}
        }
    }

    /// 追加一个事件（不写盘；写盘由 persistence 层监听总线完成）
    pub fn append(&mut self, kind: &str, data: Value, surface_append: bool) -> Event {
        self.append_full(kind, data, if surface_append { Some(json_append()) } else { None }, None)
    }

    pub fn append_full(
        &mut self,
        kind: &str,
        data: Value,
        surface_op: Option<Value>,
        source_event_seqs: Option<Vec<u64>>,
    ) -> Event {
        let e = Event {
            kind: kind.into(),
            seq: self.seq(),
            time: now_ms(),
            data,
            surface_op,
            source_event_seqs,
            ignorable: None,
        };
        self.apply_surface(&e);
        self.events.push(e.clone());
        e
    }

    /// 最新的 request/header 事件数据
    pub fn request_header(&self) -> Option<&Value> {
        self.events
            .iter()
            .rev()
            .find(|e| e.kind == "request/header")
            .map(|e| &e.data)
    }

    pub fn request_context(&self) -> Option<&Value> {
        self.events
            .iter()
            .rev()
            .find(|e| e.kind == "request/context")
            .map(|e| &e.data)
    }

    /// 从 surface 推导模型消息历史
    pub fn derive_messages(&self) -> Vec<Message> {
        let mut out = Vec::new();
        for seq in &self.surface {
            let Some(e) = self.events.get(*seq as usize) else { continue };
            let msg = match e.kind.as_str() {
                "system/message" | "developer/message" | "user/message" | "assistant/message" => {
                    e.data.get("message").or(Some(&e.data))
                }
                "tool/result" => e.data.get("message"),
                _ => None,
            };
            let Some(mv) = msg else { continue };
            let Ok(m) = serde_json::from_value::<Message>(mv.clone()) else { continue };
            // 空内容 assistant（只有 usage 的 max-tokens 节点）与空 system 不产生消息
            if m.content.is_empty() {
                continue;
            }
            out.push(m);
        }
        out
    }
}

fn json_append() -> Value {
    Value::String("append".into())
}

/// fork：复制前缀事件 + session/end-seed 标记（对应 buildForkSeed 的 minimal 版）
pub fn fork_seed(source: &Session, child_id: &str, boundary: Option<u64>) -> Session {
    let cut = boundary.unwrap_or_else(|| source.seq());
    let mut child = Session::new(child_id, source.header.cwd.clone());
    child.header.parent_session = Some(source.header.id.clone());
    child.header.is_seeded = true;
    for e in source.events.iter().take(cut as usize) {
        let mut e = e.clone();
        e.seq = child.seq();
        child.apply_surface(&e);
        child.events.push(e);
    }
    child.append("session/end-seed", serde_json::json!({"inherited": true}), false);
    child
}
