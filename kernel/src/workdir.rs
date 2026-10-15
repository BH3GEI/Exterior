//! 会话级工作目录（对应 dsh-working-directory + dsh-tool-working-directory）。

use crate::session::Session;
use std::path::{Path, PathBuf};

pub struct WorkingDirectory {
    /// 无原始目录时的回落（原版 defaultDirectory 默认为启动目录）
    pub default_directory: PathBuf,
}

pub fn context_text(cwd: &str) -> String {
    format!("Current working directory: {}.", serde_json::to_string(cwd).unwrap())
}

impl WorkingDirectory {
    pub fn new() -> Self {
        WorkingDirectory {
            default_directory: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
        }
    }

    /// 会话当前生效目录（无记录时取 header.cwd 或默认目录）
    pub fn get(&self, session: &Session) -> String {
        session
            .events
            .iter()
            .rev()
            .find(|e| e.kind == "working-directory/change")
            .and_then(|e| e.data.get("cwd").and_then(serde_json::Value::as_str))
            .map(String::from)
            .or_else(|| session.header.cwd.clone())
            .unwrap_or_else(|| self.default_directory.to_string_lossy().into())
    }

    /// 校验当前目录存在；消失则恢复到原始目录（对应 ensure）
    /// 返回 (生效目录, 需要提交的 change 事件)
    pub fn ensure(&self, session: &Session) -> Result<(String, Option<String>), String> {
        let current = self.get(session);
        if Path::new(&current).is_dir() {
            // 首次使用时记录
            let recorded = session.events.iter().any(|e| e.kind == "working-directory/change");
            return Ok((current.clone(), if recorded { None } else { Some(current) }));
        }
        let original = session
            .header
            .cwd
            .clone()
            .unwrap_or_else(|| self.default_directory.to_string_lossy().into());
        if !Path::new(&original).is_dir() {
            return Err(format!("working-directory: 目录不存在：{original}"));
        }
        Ok((original.clone(), Some(original)))
    }

    /// cd 到绝对路径或相对当前目录的路径（对应 set）
    pub fn set(&self, session: &Session, path: &str) -> Result<(String, Option<String>), String> {
        if path.is_empty() {
            return Err("working-directory: cd must not be empty".into());
        }
        let current = self.get(session);
        let target = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            Path::new(&current).join(path)
        };
        let canonical = target
            .canonicalize()
            .map_err(|_| format!("working-directory: directory does not exist: {path:?}"))?;
        if !canonical.is_dir() {
            return Err(format!("working-directory: directory does not exist: {path:?}"));
        }
        let dir = canonical.to_string_lossy().to_string();
        Ok((dir.clone(), Some(dir)))
    }
}

pub const WORKING_DIRECTORY_TOOL_NAME: &str = "working_directory";
pub const WORKING_DIRECTORY_TOOL_DESC: &str = "Read the current working directory, or change it with cd. Relative paths use the current directory. Existing shells and running processes keep their own directories.";
