//! JSONL 持久化：每会话一个文件，compression: none（对应 dsh-session-persistence-jsonl）。

use crate::session::{Event, Session, SessionHeader, SESSION_FORMAT_VERSION};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

pub fn dsh_home() -> PathBuf {
    if let Ok(p) = std::env::var("DSH_HOME") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".dsh")
}

/// SessionId → 路径安全编码（字母数字 . _ - 保留，其余 %XX）
pub fn encode_id(id: &str) -> String {
    let mut out = String::new();
    for b in id.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02x}"));
        }
    }
    out
}

fn session_dir(id: &str) -> PathBuf {
    dsh_home().join("sessions").join(encode_id(id))
}

fn log_path(id: &str) -> PathBuf {
    session_dir(id).join(format!("session.v{SESSION_FORMAT_VERSION}.jsonl"))
}

fn header_line(h: &SessionHeader) -> Value {
    let mut v = json!({
        "type": "session",
        "version": h.version,
        "id": h.id,
        "createdAt": h.created_at,
        "isSeeded": h.is_seeded,
        "delegationDepth": h.delegation_depth,
    });
    if let Some(cwd) = &h.cwd {
        v["cwd"] = json!(cwd);
    }
    if let Some(p) = &h.parent_session {
        v["parentSession"] = json!(p);
    }
    v
}

fn parse_header(v: &Value) -> Option<SessionHeader> {
    if v.get("type")?.as_str()? != "session" {
        return None;
    }
    Some(SessionHeader {
        version: v.get("version")?.as_u64()? as u32,
        id: v.get("id")?.as_str()?.into(),
        created_at: v.get("createdAt")?.as_i64()?,
        cwd: v.get("cwd").and_then(Value::as_str).map(String::from),
        parent_session: v.get("parentSession").and_then(Value::as_str).map(String::from),
        is_seeded: v.get("isSeeded")?.as_bool()?,
        delegation_depth: v.get("delegationDepth").and_then(Value::as_u64).unwrap_or(0) as u32,
    })
}

/// 追加一行到会话日志（单次 write_all，避免并发追加时拆行）
pub fn append_line(id: &str, line: &Value) -> std::io::Result<()> {
    fs::create_dir_all(session_dir(id))?;
    let mut f = OpenOptions::new().create(true).append(true).open(log_path(id))?;
    let mut s = serde_json::to_string(line).unwrap();
    s.push('\n');
    f.write_all(s.as_bytes())
}

pub fn persist_header(session: &Session) -> std::io::Result<()> {
    append_line(&session.header.id, &header_line(&session.header))
}

pub fn persist_event(id: &str, e: &Event) -> std::io::Result<()> {
    append_line(id, &serde_json::to_value(e).unwrap())
}

/// 按 sessionId 恢复；文件不存在返回 None
pub fn load(id: &str) -> Option<Session> {
    let path = log_path(id);
    if !path.exists() {
        return None;
    }
    let f = fs::File::open(path).ok()?;
    let mut header = None;
    let mut events = Vec::new();
    for line in BufReader::new(f).lines() {
        let line = line.ok()?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if header.is_none() {
            header = parse_header(&v);
            continue;
        }
        if let Ok(e) = serde_json::from_value::<Event>(v) {
            events.push(e);
        }
    }
    let mut s = Session::restore(header?, events);
    s.header.id = id.into();
    Some(s)
}
