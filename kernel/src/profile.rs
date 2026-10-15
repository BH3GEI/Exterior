//! 内置 sdk-minimal profile（与 cordis.patch.yml 的行一一对应）。

use crate::cordis::PluginRow;
use serde_json::json;

pub fn sdk_minimal() -> Vec<PluginRow> {
    let rows: Vec<(&str, &str, &[&str], serde_json::Value)> = vec![
        ("sdk-app-startup", "sdk-app", &[], json!({"profile": "sdk-minimal"})),
        ("llm", "llm", &[], json!({})),
        ("llm-retry", "llm-retry", &["llm"], json!({})),
        ("session", "session", &[], json!({})),
        ("session-projection", "session-projection", &["session"], json!({})),
        ("sessions", "session-persistence-jsonl", &["session"], json!({"compression": "none"})),
        ("session-title", "session-title", &["session", "agents"], json!({
            "fallbackMaxWords": 5, "fallbackMaxBytes": 40, "maxTitleBytes": 80
        })),
        ("system-prompt", "system-prompt", &[], json!({})),
        ("working-directory", "working-directory", &["session"], json!({})),
        ("tool-working-directory", "tool-working-directory", &["workingDirectory", "tools"], json!({})),
        ("tools", "tools", &[], json!({})),
        ("terminal-bash", "terminal-bash", &[], json!({"timeoutMs": 300000})),
        ("persistent-bash", "persistent-bash", &["tools", "terminals", "workingDirectory"], json!({
            "timeoutMs": 300000
        })),
        ("timer", "timer", &[], json!({})),
        ("jobs", "jobs", &[], json!({})),
        ("fs-local", "fs-local", &[], json!({})),
        ("sandbox", "sandbox-local", &[], json!({})),
        ("sandbox-policy", "sandbox-policy", &["fs"], json!({"mode": "danger-full-access"})),
        ("mcp-resources", "mcp-resources", &[], json!({})),
        ("agent", "agent", &["session", "llm", "tools", "terminals", "workingDirectory", "persistentBashCfg"], json!({})),
        ("agent-loop", "agent-loop", &["agents", "tools", "systemPrompt"], json!({"agents": []})),
        ("sdk-jsonrpc-server", "sdk-jsonrpc-server", &["agents", "workingDirectory"], json!({"maxTokensAsSuccess": false})),
    ];
    rows.into_iter()
        .map(|(id, name, inject, config)| PluginRow {
            id: id.into(),
            name: name.into(),
            config,
            disabled: false,
            inject: inject.iter().map(|s| s.to_string()).collect(),
        })
        .collect()
}

/// Exterior 默认 profile：在 sdk-minimal 基础上关闭 working_directory 工具（SPEC §0.4）
pub fn exterior_default() -> Vec<PluginRow> {
    let mut rows = sdk_minimal();
    for r in &mut rows {
        if r.id == "tool-working-directory" {
            r.disabled = true;
        }
    }
    rows
}
