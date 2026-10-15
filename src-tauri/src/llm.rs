use futures_util::StreamExt;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct ToolCallReq {
    pub id: String,
    pub cmd: String,
}

#[derive(Default)]
pub struct StreamResult {
    pub text: String,
    pub tool_calls: Vec<ToolCallReq>,
    pub aborted: bool,
}

/// OpenAI 兼容 Chat Completions 流式调用（SPEC §2.3 / §3.2）
pub async fn stream_chat(
    client: &reqwest::Client,
    base_url: &str,
    api_key: Option<&str>,
    model: &str,
    messages: Vec<Value>,
    cancel: Arc<AtomicBool>,
    mut on_delta: impl FnMut(&str),
) -> Result<StreamResult, String> {
    let body = json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "tools": [{
            "type": "function",
            "function": {
                "name": "bash",
                "description": "Run a bash command on the user's machine (bash -lc).",
                "parameters": {
                    "type": "object",
                    "properties": {"command": {"type": "string"}},
                    "required": ["command"]
                }
            }
        }]
    });
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let mut req = client.post(&url).json(&body);
    if let Some(k) = api_key {
        req = req.bearer_auth(k);
    }
    let resp = req.send().await.map_err(|e| format!("请求失败：{e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("模型返回 {status}：{text}"));
    }

    let mut result = StreamResult::default();
    let mut args: Vec<String> = Vec::new(); // 按 index 累积 arguments
    let mut ids: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::Relaxed) {
            result.aborted = true;
            return Ok(result);
        }
        let chunk = chunk.map_err(|e| format!("读取流失败：{e}"))?;
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buf.find('\n') {
            let line = buf[..pos].trim().to_string();
            buf.drain(..=pos);
            let Some(data) = line.strip_prefix("data:") else { continue };
            let data = data.trim();
            if data == "[DONE]" {
                return Ok(finish(result, ids, args));
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
            for choice in v["choices"].as_array().into_iter().flatten() {
                let delta = &choice["delta"];
                if let Some(c) = delta["content"].as_str() {
                    result.text.push_str(c);
                    on_delta(c);
                }
                for tc in delta["tool_calls"].as_array().into_iter().flatten() {
                    let idx = tc["index"].as_u64().unwrap_or(0) as usize;
                    while ids.len() <= idx {
                        ids.push(String::new());
                        args.push(String::new());
                    }
                    if let Some(id) = tc["id"].as_str() {
                        ids[idx].push_str(id);
                    }
                    if let Some(a) = tc["function"]["arguments"].as_str() {
                        args[idx].push_str(a);
                    }
                }
            }
        }
    }
    Ok(finish(result, ids, args))
}

fn finish(mut result: StreamResult, ids: Vec<String>, args: Vec<String>) -> StreamResult {
    for (id, a) in ids.into_iter().zip(args) {
        let cmd = serde_json::from_str::<Value>(&a)
            .ok()
            .and_then(|v| v["command"].as_str().map(String::from))
            .unwrap_or(a);
        if !id.is_empty() {
            result.tool_calls.push(ToolCallReq { id, cmd });
        }
    }
    result
}
