//! 系统提示组装（对应 dsh-system-prompt，minimal profile 下只有 persona）+
//! runtime context 快照（"Current runtime context. ..." 文本）。

/// sdk-minimal：includeHarnessIdentity=false、includeRuntimeContext=false，
/// 系统提示就是 persona（DSH_SYSTEM_PROMPT 或默认值）
pub fn persona() -> String {
    std::env::var("DSH_SYSTEM_PROMPT")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "You are a helpful software engineer assistant.".into())
}

/// runtime context 快照文本（对应 joinContextSections）
pub fn context_snapshot(sections: &[String]) -> String {
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

/// 快照清空时的文本（对应 runtime-context.ts 的 CLEARED）
pub const CONTEXT_CLEARED: &str =
    "Current runtime context: none. Earlier runtime-context snapshots no longer apply.";
