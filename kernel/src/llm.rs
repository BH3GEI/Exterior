//! LLM 类型与 OpenAI 兼容 Chat Completions 客户端（流式 + tool calling）。
//! 对应原版 dsh-llm + dsh-llm-deepseek-api-key，接 OpenAI 兼容接口，DeepSeek 是其中一种配置。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

// ---------- 内容块与消息（对应 dsh-llm 的 ContentBlock / Message） ----------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "reasoning")]
    Reasoning {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    #[serde(rename = "tool-call")]
    ToolCall { id: String, name: String, arguments: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    pub role: String, // system | developer | user | assistant | tool
    pub content: Vec<ContentBlock>,
    pub source: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn create_user_message(text: &str, source: Value) -> Message {
    Message {
        id: new_id(),
        role: "user".into(),
        content: vec![ContentBlock::Text { text: text.into() }],
        source,
        tool_call_id: None,
        is_error: None,
    }
}

pub fn create_system_message(text: &str) -> Message {
    Message {
        id: new_id(),
        role: "system".into(),
        content: if text.is_empty() {
            vec![]
        } else {
            vec![ContentBlock::Text { text: text.into() }]
        },
        source: json!({"kind": "system-prompt"}),
        tool_call_id: None,
        is_error: None,
    }
}

pub fn create_assistant_message(content: Vec<ContentBlock>, provider: &str, model: &str) -> Message {
    Message {
        id: new_id(),
        role: "assistant".into(),
        content,
        source: json!({"kind": "model", "provider": provider, "model": model}),
        tool_call_id: None,
        is_error: None,
    }
}

pub fn create_tool_result_message(call_id: &str, text: &str, is_error: bool) -> Message {
    Message {
        id: new_id(),
        role: "tool".into(),
        content: vec![ContentBlock::Text { text: text.into() }],
        source: json!({"kind": "tool", "callId": call_id}),
        tool_call_id: Some(call_id.into()),
        is_error: if is_error { Some(true) } else { None },
    }
}

// ---------- 工具 schema ----------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

// ---------- 失败 ----------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LlmFailure {
    pub message: String,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

pub const EMPTY_RESPONSE: &str = "EMPTY_RESPONSE";
pub const RATE_LIMIT: &str = "RATE_LIMIT";
pub const SERVER: &str = "SERVER";
pub const TIMEOUT: &str = "TIMEOUT";
pub const TRANSPORT: &str = "TRANSPORT";

pub const RETRYABLE_CODES: [&str; 5] = [EMPTY_RESPONSE, RATE_LIMIT, SERVER, TIMEOUT, TRANSPORT];

// ---------- Token 用量 ----------

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
}

// ---------- 重试策略（默认值对应原版 retry-policy） ----------

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryPolicy {
    #[serde(default = "d_max_retries")]
    pub max_retries: u32,
    #[serde(default = "d_initial_delay")]
    pub initial_delay_ms: u64,
    #[serde(default = "d_max_delay")]
    pub max_delay_ms: u64,
    #[serde(default = "d_jitter")]
    pub jitter_ratio: f64,
    #[serde(default = "d_codes")]
    pub retryable_codes: Vec<String>,
}
fn d_max_retries() -> u32 { 5 }
fn d_initial_delay() -> u64 { 500 }
fn d_max_delay() -> u64 { 10_000 }
fn d_jitter() -> f64 { 0.1 }
fn d_codes() -> Vec<String> { RETRYABLE_CODES.iter().map(|s| (*s).into()).collect() }

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_retries: 5,
            initial_delay_ms: 500,
            max_delay_ms: 10_000,
            jitter_ratio: 0.1,
            retryable_codes: d_codes(),
        }
    }
}

impl RetryPolicy {
    /// 原版 localDelay：指数退避 + 抖动
    pub fn delay_ms(&self, retry: u32, rand01: f64) -> u64 {
        let exponent = (retry.saturating_sub(1)).min(1024);
        let exponential = (self.initial_delay_ms as f64) * 2f64.powi(exponent as i32);
        let jitter = 1.0 - self.jitter_ratio + 2.0 * self.jitter_ratio * rand01;
        (exponential.min(self.max_delay_ms as f64) * jitter).min(self.max_delay_ms as f64) as u64
    }
}

// ---------- 流式结果 ----------

/// 一个 assistant 尝试的产出（对应 AssistantStreamRecord 的语义层）
#[derive(Default, Debug)]
pub struct StreamOutcome {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<(String, String, String)>, // (id, name, arguments)
    pub usage: Option<TokenUsage>,
    pub finish: FinishKind,
    /// 紧凑流记录（assistant/message 事件的 stream 字段）
    pub stream: Vec<Value>,
    pub aborted: bool,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub enum FinishKind {
    #[default]
    Stop,
    ToolCalls,
    MaxTokens,
}

impl StreamOutcome {
    /// 组装 assistant 消息的内容块（text/reasoning/tool-call，按出现顺序：reasoning 先、text 后、tool calls 最后）
    pub fn blocks(&self) -> Vec<ContentBlock> {
        let mut v = Vec::new();
        if !self.reasoning.is_empty() {
            v.push(ContentBlock::Reasoning { text: self.reasoning.clone(), signature: None });
        }
        if !self.text.is_empty() {
            v.push(ContentBlock::Text { text: self.text.clone() });
        }
        for (id, name, arguments) in &self.tool_calls {
            v.push(ContentBlock::ToolCall { id: id.clone(), name: name.clone(), arguments: arguments.clone() });
        }
        v
    }
}

// ---------- OpenAI 兼容 wire 转换 ----------

/// 把推导出的消息序列转成 OpenAI Chat Completions 消息。
/// 连续的 user 消息合并为一条（content 为 parts 数组，逐块保留），
/// 与原版 DeepSeek 适配器把 prompt + runtime context 合并为一条 user 消息一致。
pub fn to_wire_messages(messages: &[Message]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut pending_user: Vec<Value> = Vec::new(); // user parts 暂存

    let flush_user = |out: &mut Vec<Value>, parts: &mut Vec<Value>| {
        if !parts.is_empty() {
            out.push(json!({"role": "user", "content": std::mem::take(parts)}));
        }
    };

    for m in messages {
        match m.role.as_str() {
            "system" => {
                flush_user(&mut out, &mut pending_user);
                let text = m.content.iter().find_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                });
                if let Some(t) = text {
                    if !t.is_empty() {
                        out.insert(0, json!({"role": "system", "content": t}));
                    }
                }
            }
            "user" => {
                for b in &m.content {
                    if let ContentBlock::Text { text } = b {
                        if !text.is_empty() {
                            pending_user.push(json!({"type": "text", "text": text}));
                        }
                    }
                }
            }
            "assistant" => {
                flush_user(&mut out, &mut pending_user);
                let text: String = m.content.iter().filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                }).collect();
                let calls: Vec<&ContentBlock> = m.content.iter().filter(|b| matches!(b, ContentBlock::ToolCall { .. })).collect();
                let mut msg = json!({"role": "assistant"});
                if text.is_empty() && !calls.is_empty() {
                    msg["content"] = Value::Null;
                } else {
                    msg["content"] = json!(text);
                }
                if !calls.is_empty() {
                    msg["tool_calls"] = json!(calls.iter().map(|c| match c {
                        ContentBlock::ToolCall { id, name, arguments } => json!({
                            "id": id, "type": "function",
                            "function": {"name": name, "arguments": arguments}
                        }),
                        _ => unreachable!(),
                    }).collect::<Vec<_>>());
                }
                out.push(msg);
            }
            "tool" => {
                flush_user(&mut out, &mut pending_user);
                let text: String = m.content.iter().filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                }).collect();
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": text,
                }));
            }
            _ => {} // developer 消息在 minimal profile 不出现
        }
    }
    flush_user(&mut out, &mut pending_user);
    out
}

pub fn to_wire_tools(tools: &[ToolSchema]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| json!({
            "type": "function",
            "function": {"name": t.name, "description": t.description, "parameters": t.parameters}
        }))
        .collect()
}

// ---------- 模型提供方配置 ----------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderCfg {
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub retry_policy: Option<RetryPolicy>,
}

/// $DSH_HOME/config.json
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LlmConfig {
    #[serde(default)]
    pub providers: Vec<ProviderCfg>,
}

pub fn read_llm_config() -> Result<LlmConfig, String> {
    let path = crate::persist::dsh_home().join("config.json");
    if !path.exists() {
        return Ok(LlmConfig::default());
    }
    let s = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    serde_json::from_str(&s).map_err(|e| format!("config.json 解析失败：{e}"))
}

// ---------- 流式调用 ----------

pub struct StreamCtx {
    pub provider: ProviderCfg,
    pub model: String,
    pub max_tokens: Option<u64>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 一次流式调用。cancel 置位后保留已流出的部分返回（aborted=true）。
pub async fn stream_chat(
    http: &reqwest::Client,
    ctx: &StreamCtx,
    wire_messages: Vec<Value>,
    wire_tools: Vec<Value>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    mut on_delta: impl FnMut(&str),
) -> Result<StreamOutcome, LlmFailure> {
    use std::sync::atomic::Ordering;
    let mut body = json!({
        "model": ctx.model,
        "messages": wire_messages,
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if !wire_tools.is_empty() {
        body["tools"] = json!(wire_tools);
    }
    if let Some(mt) = ctx.max_tokens {
        body["max_tokens"] = json!(mt);
    }
    let url = format!("{}/chat/completions", ctx.provider.base_url.trim_end_matches('/'));
    let mut req = http.post(&url).json(&body);
    if let Some(env) = &ctx.provider.api_key_env {
        if let Ok(key) = std::env::var(env) {
            req = req.bearer_auth(key);
        }
    }
    let resp = req.send().await.map_err(|e| LlmFailure {
        message: format!("请求失败：{e}"),
        code: if e.is_timeout() { TIMEOUT } else { TRANSPORT }.into(),
        status: None,
    })?;
    let status = resp.status();
    if !status.is_success() {
        let code = match status.as_u16() {
            429 => RATE_LIMIT,
            s if s >= 500 => SERVER,
            _ => "INVALID_REQUEST",
        };
        let text = resp.text().await.unwrap_or_default();
        return Err(LlmFailure {
            message: format!("模型返回 {}：{}", status.as_u16(), text.chars().take(500).collect::<String>()),
            code: code.into(),
            status: Some(status.as_u16()),
        });
    }

    let mut out = StreamOutcome::default();
    let mut aborted = false;
    // 紧凑流记录：按块累积
    let mut text_dt: Vec<i64> = vec![];
    let mut text_texts: Vec<String> = vec![];
    let mut text_time0 = 0i64;
    let mut last_t = 0i64;
    let mut tc_records: Vec<(usize, String, String, Vec<String>)> = vec![]; // index, id, name, arg chunks

    let mut buf = String::new();
    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::Relaxed) {
            aborted = true;
            break;
        }
        let chunk = chunk.map_err(|e| LlmFailure {
            message: format!("读取流失败：{e}"),
            code: TRANSPORT.into(),
            status: None,
        })?;
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buf.find('\n') {
            let line = buf[..pos].trim().to_string();
            buf.drain(..=pos);
            let Some(data) = line.strip_prefix("data:") else { continue };
            let data = data.trim();
            if data == "[DONE]" {
                break;
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
            if let Some(u) = v.get("usage") {
                let inp = u["prompt_tokens"].as_u64().unwrap_or(0);
                let outp = u["completion_tokens"].as_u64().unwrap_or(0);
                out.usage = Some(TokenUsage {
                    input_tokens: inp,
                    output_tokens: outp,
                    total_tokens: u["total_tokens"].as_u64().or(Some(inp + outp)),
                });
            }
            for choice in v["choices"].as_array().into_iter().flatten() {
                if let Some(fr) = choice["finish_reason"].as_str() {
                    out.finish = match fr {
                        "tool_calls" => FinishKind::ToolCalls,
                        "length" => FinishKind::MaxTokens,
                        _ => FinishKind::Stop,
                    };
                }
                let delta = &choice["delta"];
                let now = now_ms();
                if let Some(c) = delta["content"].as_str() {
                    if text_time0 == 0 {
                        text_time0 = now;
                    }
                    text_dt.push(now - last_t.max(text_time0));
                    last_t = now;
                    text_texts.push(c.to_string());
                    out.text.push_str(c);
                    on_delta(c);
                }
                if let Some(c) = delta["reasoning_content"].as_str() {
                    out.reasoning.push_str(c);
                }
                for tc in delta["tool_calls"].as_array().into_iter().flatten() {
                    let idx = tc["index"].as_u64().unwrap_or(0) as usize;
                    while out.tool_calls.len() <= idx {
                        out.tool_calls.push((String::new(), String::new(), String::new()));
                    }
                    while tc_records.len() <= idx {
                        tc_records.push((idx, String::new(), String::new(), vec![]));
                    }
                    if let Some(id) = tc["id"].as_str() {
                        out.tool_calls[idx].0.push_str(id);
                        tc_records[idx].1.push_str(id);
                    }
                    if let Some(name) = tc["function"]["name"].as_str() {
                        out.tool_calls[idx].1.push_str(name);
                        tc_records[idx].2.push_str(name);
                    }
                    if let Some(a) = tc["function"]["arguments"].as_str() {
                        out.tool_calls[idx].2.push_str(a);
                        tc_records[idx].3.push(a.to_string());
                    }
                }
            }
        }
    }

    // 组装紧凑流记录
    if !text_texts.is_empty() {
        out.stream.push(json!({
            "type": "text-chunks", "time0": text_time0, "index": 0,
            "dt": text_dt, "texts": text_texts,
        }));
    }
    for (i, (_idx, id, name, args)) in tc_records.iter().enumerate() {
        if !id.is_empty() {
            out.stream.push(json!({
                "type": "tool-call-chunks", "time0": now_ms(), "index": i,
                "dt": [], "id": id, "name": name, "args": args,
            }));
        }
    }

    if aborted {
        out.aborted = true;
        return Ok(out);
    }
    if out.text.is_empty() && out.tool_calls.is_empty() && out.reasoning.is_empty() {
        return Err(LlmFailure {
            message: "模型返回了空响应".into(),
            code: EMPTY_RESPONSE.into(),
            status: None,
        });
    }
    Ok(out)
}

