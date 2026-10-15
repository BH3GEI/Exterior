//! 系统提示服务（对应 dsh-system-prompt，minimal profile 下只有 persona）。
//! 服务接口 + 默认实现；runtime context 快照文本在此组装。

/// 服务接口（cordis 服务名 "systemPrompt"）
pub trait SystemPromptService: Send + Sync {
    /// 系统提示文本。sdk-minimal：persona = DSH_SYSTEM_PROMPT 或默认值
    fn persona(&self) -> String;
    /// runtime context 快照（对应 joinContextSections）
    fn context_snapshot(&self, sections: &[String]) -> String;
    /// 快照清空时的文本
    fn context_cleared(&self) -> String;
}

pub struct DefaultSystemPrompt;

impl SystemPromptService for DefaultSystemPrompt {
    fn persona(&self) -> String {
        std::env::var("DSH_SYSTEM_PROMPT")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "You are a helpful software engineer assistant.".into())
    }

    fn context_snapshot(&self, sections: &[String]) -> String {
        let body = sections
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n");
        if body.is_empty() {
            return String::new();
        }
        format!("Current runtime context. This snapshot supersedes earlier runtime-context snapshots.\n\n{body}")
    }

    fn context_cleared(&self) -> String {
        "Current runtime context: none. Earlier runtime-context snapshots no longer apply.".into()
    }
}
