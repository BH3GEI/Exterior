//! 会话标题（对应 dsh-session-title）：无 provider 时用确定性 fallback。
//! fallbackMaxWords: 5, fallbackMaxBytes: 40, maxTitleBytes: 80

use crate::agent::AgentManager;
use crate::cordis::KernelEvent;
use serde_json::json;
use std::sync::Arc;

#[derive(Clone)]
pub struct TitleConfig {
    pub fallback_max_words: usize,
    pub fallback_max_bytes: usize,
    pub max_title_bytes: usize,
}

impl Default for TitleConfig {
    fn default() -> Self {
        TitleConfig { fallback_max_words: 5, fallback_max_bytes: 40, max_title_bytes: 80 }
    }
}

/// 对应 normalize.ts：去 ANSI/控制字符、压空白
fn clean_title_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI / OSC / ESC 序列
            match chars.peek() {
                Some('[') => {
                    for c2 in chars.by_ref() {
                        if ('@'..='~').contains(&c2) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    let mut prev = '\0';
                    for c2 in chars.by_ref() {
                        if c2 == '\u{7}' || (prev == '\u{1b}' && c2 == '\\') {
                            break;
                        }
                        prev = c2;
                    }
                }
                _ => {
                    chars.next();
                }
            }
        } else if c.is_control() || ('\u{200b}'..='\u{200f}').contains(&c) || ('\u{202a}'..='\u{202e}').contains(&c) {
            // 丢弃
        } else {
            out.push(c);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_utf8(input: &str, max_bytes: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for c in input.chars() {
        let b = c.len_utf8();
        if used + b > max_bytes {
            break;
        }
        out.push(c);
        used += b;
    }
    out
}

pub fn normalize_title(input: &str, max_bytes: usize) -> String {
    truncate_utf8(&clean_title_text(input), max_bytes).trim_end().to_string()
}

pub fn fallback_title(input: &str, max_words: usize, max_bytes: usize) -> String {
    let cleaned = clean_title_text(input);
    let words: Vec<&str> = cleaned.split(' ').filter(|w| !w.is_empty()).take(max_words).collect();
    truncate_utf8(&words.join(" "), max_bytes).trim_end().to_string()
}

/// 启动标题插件：监听 session/event，首个用户消息生成 fallback 标题
pub fn start_title_plugin(mgr: Arc<AgentManager>, config: TitleConfig) {
    let mut rx = mgr.bus.subscribe();
    tokio::spawn(async move {
        loop {
            let Ok(ev) = rx.recv().await else { continue };
            let KernelEvent::SessionEvent { session_id, event } = ev else { continue };
            if event.kind != "user/message" {
                continue;
            }
            if event.data.get("source").and_then(|s| s.get("kind")).and_then(|k| k.as_str()) != Some("user") {
                continue;
            }
            let Some(agent) = mgr.get(&session_id) else { continue };
            let text: String = {
                let session = agent.session.lock().unwrap();
                // 已有标题则不再生成
                if session.events.iter().any(|e| e.kind == "session/title") {
                    continue;
                }
                event.data.get("content")
                    .and_then(|c| c.as_array())
                    .map(|blocks| {
                        blocks.iter()
                            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default()
            };
            let title = fallback_title(&text, config.fallback_max_words, config.fallback_max_bytes);
            if title.is_empty() {
                continue;
            }
            mgr.append(
                &session_id,
                "session/title",
                json!({"title": title, "messageSeqs": [event.seq], "source": {"kind": "fallback"}}),
                false,
            );
        }
    });
}
