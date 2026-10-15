//! 持久 bash 工具（对应 dsh-tool-bash-persistent + terminal-bash + subprocess-local）。
//! 每个 agent 一个持久 shell，状态跨调用保留；同一 agent 的命令串行执行。
//! 实现用管道而非 PTY：标记协议与输出结果与原版一致（详见 PR 说明）。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub const TOOL_NAME: &str = "bash";
pub const TOOL_DESC: &str = "Run commands in a bash shell\n* When invoking this tool, the contents of the \"command\" parameter does NOT need to be XML-escaped.\n* Network access depends on the task environment. Prefer configured mirrors/proxies when they are available.\n* State is persistent across command calls and discussions with the user.\n* To inspect a particular line range of a file, e.g. lines 10-25, try 'sed -n 10,25p /path/to/the/file'.\n* Please avoid commands that may produce a very large amount of output.\n* Please run long lived commands in the background, e.g. 'sleep 10 &' or start a server in the background.";

const TRUNCATED_MESSAGE: &str = "<response clipped><NOTE>To save on context only part of this file has been shown to you. You should retry this tool after you have searched inside the file with `grep -n` in order to find the line numbers of what you are looking for.</NOTE>";
const SHELL_RESET_MESSAGE: &str = "The persistent bash shell was reset; the next bash call starts from the workspace with a fresh current directory and environment.";
const TIMEOUT_STATUS_MARKER: &str = "[Command timed out or OOM]";

pub const DEFAULT_TIMEOUT_MS: u64 = 300_000;
pub const DEFAULT_MAX_OUTPUT_CHARS: usize = 16_000;

#[derive(Clone, Copy)]
pub struct BashConfig {
    pub timeout_ms: u64,
    pub max_output_chars: usize,
}

impl Default for BashConfig {
    fn default() -> Self {
        BashConfig { timeout_ms: DEFAULT_TIMEOUT_MS, max_output_chars: DEFAULT_MAX_OUTPUT_CHARS }
    }
}

pub fn config_from(config: &Value) -> BashConfig {
    BashConfig {
        timeout_ms: config.get("timeoutMs").and_then(Value::as_u64).unwrap_or(DEFAULT_TIMEOUT_MS),
        max_output_chars: config
            .get("maxOutputChars")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(DEFAULT_MAX_OUTPUT_CHARS),
    }
}

struct Shell {
    child: Child,
    stdin: ChildStdin,
    buf: Arc<Mutex<String>>,
}

fn spawn_shell(cwd: &str) -> Result<Shell, String> {
    let mut child = Command::new("bash")
        .arg("--norc")
        .arg("--noprofile")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("启动 bash 失败：{e}"))?;
    let stdin = child.stdin.take().unwrap();
    let buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let mut readers: Vec<Box<dyn std::io::Read + Send>> = vec![
        Box::new(child.stdout.take().unwrap()),
        Box::new(child.stderr.take().unwrap()),
    ];
    for reader in readers.drain(..) {
        let buf = buf.clone();
        let mut s = reader;
        std::thread::spawn(move || {
            let mut tmp = [0u8; 8192];
            loop {
                match s.read(&mut tmp) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.lock().unwrap().push_str(&String::from_utf8_lossy(&tmp[..n])),
                }
            }
        });
    }
    Ok(Shell { child, stdin, buf })
}

fn kill_shell(mut sh: Shell) {
    let pid = sh.child.id() as i32;
    unsafe { libc::killpg(pid, libc::SIGKILL) };
    let _ = sh.child.wait();
}

fn quote_for_bash(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\r', "\\r")
        .replace('\n', "\\n");
    format!("$'{escaped}'")
}

/// 与原版 wrapCommand 一致：单行，eval，捕获退出码
fn wrap_command(command: &str, start: &str, end: &str) -> String {
    format!(
        "printf '%s\\n' {}; eval -- {}; __dsh_persistent_bash_status=$?; printf '%s%s\\n' {} \"$__dsh_persistent_bash_status\"",
        quote_for_bash(start),
        quote_for_bash(command),
        quote_for_bash(end),
    )
}

fn trim_trailing_newline(text: &str) -> &str {
    text.trim_end_matches(['\r', '\n'])
}

fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn maybe_truncate(content: &str, max: usize) -> String {
    if content.chars().count() <= max {
        content.to_string()
    } else {
        truncate_chars(content, max) + TRUNCATED_MESSAGE
    }
}

fn append_status_marker(content: String, marker: &str) -> String {
    if content.is_empty() {
        marker.to_string()
    } else {
        format!("{content}\n{marker}")
    }
}

/// 在缓冲中定位本次命令的输出：start 标记之后、end 标记行之前。
/// 返回 (输出文本, 退出码)；end 标记未出现返回 None。
fn extract_output(text: &str, start_marker: &str, end_marker: &str) -> Option<(String, i64)> {
    let end_pos = text.rfind(end_marker)?;
    let rest = &text[end_pos + end_marker.len()..];
    let line_end = rest.find('\n')?;
    let code: i64 = rest[..line_end].trim().parse().ok()?;
    let start_pos = text.rfind(start_marker).filter(|p| *p < end_pos)?;
    let mut from = start_pos + start_marker.len();
    if text.as_bytes().get(from) == Some(&b'\r') {
        from += 1;
    }
    if text.as_bytes().get(from) == Some(&b'\n') {
        from += 1;
    }
    let mut to = end_pos;
    if to > from && text.as_bytes()[to - 1] == b'\n' {
        to -= 1;
        if to > from && text.as_bytes()[to - 1] == b'\r' {
            to -= 1;
        }
    }
    Some((text[from.min(to)..to.max(from)].to_string(), code))
}

/// 超时/shell 退出路径：start 标记之后、end 标记残片之前的部分输出
fn extract_partial(text: &str, start_marker: &str) -> String {
    let Some(pos) = text.rfind(start_marker) else {
        return String::new();
    };
    let mut from = pos + start_marker.len();
    if text.as_bytes().get(from) == Some(&b'\r') {
        from += 1;
    }
    if text.as_bytes().get(from) == Some(&b'\n') {
        from += 1;
    }
    let mut to = text.len();
    if let Some(e) = text[from..].rfind("__DSH_PERSISTENT_BASH_END_") {
        to = from + e;
    }
    trim_trailing_newline(&text[from..to]).to_string()
}

/// bash 工具服务：每会话一个持久 shell，命令串行执行
pub struct BashService {
    shells: Mutex<HashMap<String, Arc<Mutex<Option<Shell>>>>>,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl BashService {
    pub fn new() -> Self {
        BashService { shells: Mutex::new(HashMap::new()), locks: Mutex::new(HashMap::new()) }
    }

    pub fn kill_all(&self) {
        let shells: Vec<_> = self.shells.lock().unwrap().values().cloned().collect();
        for s in shells {
            if let Some(sh) = s.lock().unwrap().take() {
                kill_shell(sh);
            }
        }
    }

    fn reset(&self, session: &str) {
        if let Some(cell) = self.shells.lock().unwrap().get(session).cloned() {
            if let Some(sh) = cell.lock().unwrap().take() {
                kill_shell(sh);
            }
        }
    }

    fn serial_lock(&self, session: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .lock()
            .unwrap()
            .entry(session.into())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// 执行一条命令，返回模型可见文本（含 [Command finished with exit code N]）与退出码
    pub async fn execute(
        &self,
        session: &str,
        command: &str,
        cwd: &str,
        config: BashConfig,
        cancel: Arc<AtomicBool>,
        on_chunk: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    ) -> Result<(String, Option<i64>), String> {
        if command.trim().is_empty() {
            return Err("command must be a non-empty string".into());
        }
        let serial = self.serial_lock(session);
        let _serial = serial.lock().await;
        let cell = self
            .shells
            .lock()
            .unwrap()
            .entry(session.into())
            .or_insert_with(|| Arc::new(Mutex::new(None)))
            .clone();

        // 取或建 shell；发现已退出则带上退出状态并重建
        let mut shell_exit_marker: Option<String> = None;
        {
            let mut guard = cell.lock().unwrap();
            let alive = guard
                .as_mut()
                .map(|sh| sh.child.try_wait().ok().flatten().is_none())
                .unwrap_or(false);
            if !alive {
                if let Some(mut sh) = guard.take() {
                    let code = sh.child.try_wait().ok().flatten().and_then(|s| s.code());
                    shell_exit_marker = Some(match code {
                        Some(c) => format!("[shell exited: code {c}]"),
                        None => "[shell exited]".into(),
                    });
                }
                *guard = Some(spawn_shell(cwd)?);
            }
        }

        let nonce = uuid::Uuid::new_v4().to_string();
        let start_marker = format!("__DSH_PERSISTENT_BASH_START_{nonce}__");
        let end_marker = format!("__DSH_PERSISTENT_BASH_END_{nonce}:");

        let buf = cell.lock().unwrap().as_ref().unwrap().buf.clone();
        let wrapped = wrap_command(command, &start_marker, &end_marker);
        {
            let mut guard = cell.lock().unwrap();
            let sh = guard.as_mut().unwrap();
            sh.stdin.write_all(wrapped.as_bytes()).map_err(|e| e.to_string())?;
            sh.stdin.write_all(b"\n").map_err(|e| e.to_string())?;
            sh.stdin.flush().map_err(|e| e.to_string())?;
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(config.timeout_ms);
        enum Outcome {
            Done(String, i64),
            Timeout(String),
            Exited(String, Option<i64>),
            Aborted,
        }
        let mut pushed = 0usize;
        let outcome = loop {
            if cancel.load(Ordering::Relaxed) {
                break Outcome::Aborted;
            }
            let text = buf.lock().unwrap().clone();
            if let Some(cb) = &on_chunk {
                if text.len() > pushed {
                    cb(&text[pushed..]);
                    pushed = text.len();
                }
            }
            if let Some((out, code)) = extract_output(&text, &start_marker, &end_marker) {
                break Outcome::Done(out, code);
            }
            let exited = {
                let mut guard = cell.lock().unwrap();
                guard.as_mut().and_then(|sh| sh.child.try_wait().ok().flatten()).map(|s| s.code())
            };
            if cell.lock().unwrap().is_none() || exited.is_some() {
                break Outcome::Exited(extract_partial(&text, &start_marker), exited.flatten().map(|c| c as i64));
            }
            if std::time::Instant::now() >= deadline {
                break Outcome::Timeout(extract_partial(&text, &start_marker));
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        };

        match outcome {
            Outcome::Done(body, code) => {
                let mut rendered = maybe_truncate(trim_trailing_newline(&body), config.max_output_chars);
                if let Some(note) = shell_exit_marker {
                    rendered = append_status_marker(rendered, &note);
                }
                Ok((append_status_marker(rendered, &format!("[Command finished with exit code {code}]")), Some(code)))
            }
            Outcome::Timeout(partial) => {
                let rendered = maybe_truncate(&partial, config.max_output_chars);
                self.reset(session);
                Ok((format!(
                    "Your command timed out after {} seconds or experienced an OOM error. Below is partial output:\n{}\n{}",
                    config.timeout_ms / 1000,
                    append_status_marker(rendered, TIMEOUT_STATUS_MARKER),
                    SHELL_RESET_MESSAGE,
                ), None))
            }
            Outcome::Exited(partial, code) => {
                let rendered = maybe_truncate(&partial, config.max_output_chars);
                self.reset(session);
                let marker = match code {
                    Some(c) => format!("[shell exited: code {c}]"),
                    None => "[shell exited]".into(),
                };
                Ok(([append_status_marker(rendered, &marker), SHELL_RESET_MESSAGE.to_string()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n"), code.map(|c| c as i64)))
            }
            Outcome::Aborted => {
                self.reset(session);
                Ok((String::new(), None)) // 中止：空结果，由上层收尾（对应原版 abort 返回 ''）
            }
        }
    }
}

pub fn bash_tool_schema() -> crate::llm::ToolSchema {
    crate::llm::ToolSchema {
        name: TOOL_NAME.into(),
        description: TOOL_DESC.into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command to run. Relative path is preferred in the command."
                }
            },
            "required": ["command"]
        }),
    }
}

pub fn working_directory_tool_schema() -> crate::llm::ToolSchema {
    crate::llm::ToolSchema {
        name: crate::workdir::WORKING_DIRECTORY_TOOL_NAME.into(),
        description: crate::workdir::WORKING_DIRECTORY_TOOL_DESC.into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "cd": {
                    "type": "string",
                    "description": "Existing directory to enter. Omit to read the current directory."
                }
            }
        }),
    }
}
