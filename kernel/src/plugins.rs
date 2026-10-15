//! 编译期插件注册表：名字 → 启动函数（对应 cordis.patch.yml 各行）。

use crate::agent::AgentManager;
use crate::bash_tool::BashService;
use crate::cordis::{Kernel, PluginDef};
use crate::jobs::{JobRegistry, Timer};
use crate::llm::{read_llm_config, LlmConfig, RetryPolicy};
use crate::projection::LinearProjection;
use crate::session::SessionServices;
use crate::sysprompt::{DefaultSystemPrompt, SystemPromptService};
use crate::tools::ToolsRegistry;
use crate::workdir::WorkingDirectory;
use serde_json::Value;
use std::sync::Arc;

fn noop(_k: &mut Kernel, _c: &Value) -> Result<(), String> {
    Ok(())
}

fn start_session(k: &mut Kernel, _c: &Value) -> Result<(), String> {
    k.provide("session", SessionServices::default());
    Ok(())
}

fn start_llm(k: &mut Kernel, _c: &Value) -> Result<(), String> {
    let cfg = read_llm_config()?;
    k.provide("llm", cfg);
    Ok(())
}

fn start_sessions_jsonl(k: &mut Kernel, _c: &Value) -> Result<(), String> {
    let ss: Arc<SessionServices> = k.get("session").ok_or("session-persistence-jsonl 需要 session 服务")?;
    *ss.persistence.write().unwrap() = Some(Arc::new(crate::persist::JsonlPersistence));
    k.provide("sessions", ());
    Ok(())
}

fn start_sessions_memory(k: &mut Kernel, _c: &Value) -> Result<(), String> {
    let ss: Arc<SessionServices> = k.get("session").ok_or("session-persistence-memory 需要 session 服务")?;
    *ss.persistence.write().unwrap() = Some(Arc::new(crate::persist::MemoryPersistence::default()));
    k.provide("sessions", ());
    Ok(())
}

fn start_projection(k: &mut Kernel, _c: &Value) -> Result<(), String> {
    let ss: Arc<SessionServices> = k.get("session").ok_or("session-projection 需要 session 服务")?;
    *ss.projection.write().unwrap() = Some(Arc::new(LinearProjection));
    Ok(())
}

/// Exterior DAG 插件：替换投影 + 提供 dag 服务
fn start_dag(k: &mut Kernel, _c: &Value) -> Result<(), String> {
    let ss: Arc<SessionServices> = k.get("session").ok_or("exterior-dag 需要 session 服务")?;
    let dag = Arc::new(crate::dag::DagProjection::new());
    *ss.projection.write().unwrap() = Some(dag.clone());
    k.provide("dag", dag);
    Ok(())
}

fn start_sysprompt(k: &mut Kernel, _c: &Value) -> Result<(), String> {
    k.provide("systemPrompt", DefaultSystemPrompt);
    Ok(())
}

fn start_retry(k: &mut Kernel, _c: &Value) -> Result<(), String> {
    k.provide("retry", RetryPolicy::default());
    Ok(())
}

fn start_workdir(k: &mut Kernel, _c: &Value) -> Result<(), String> {
    k.provide("workingDirectory", WorkingDirectory::new());
    Ok(())
}

fn start_tool_workdir(k: &mut Kernel, _c: &Value) -> Result<(), String> {
    let tools: Arc<ToolsRegistry> = k.get("tools").ok_or("tool-working-directory 需要 tools 服务")?;
    tools.register(crate::bash_tool::working_directory_tool_schema());
    Ok(())
}

fn start_tools(k: &mut Kernel, _c: &Value) -> Result<(), String> {
    k.provide("tools", ToolsRegistry::new());
    Ok(())
}

fn start_bash(k: &mut Kernel, c: &Value) -> Result<(), String> {
    let tools: Arc<ToolsRegistry> = k.get("tools").ok_or("persistent-bash 需要 tools 服务")?;
    tools.register(crate::bash_tool::bash_tool_schema());
    k.provide("persistentBashCfg", crate::bash_tool::config_from(c));
    Ok(())
}

fn start_terminals(k: &mut Kernel, _c: &Value) -> Result<(), String> {
    k.provide("terminals", BashService::new());
    Ok(())
}

fn start_timer(k: &mut Kernel, _c: &Value) -> Result<(), String> {
    k.provide("timer", Timer);
    Ok(())
}

fn start_jobs(k: &mut Kernel, _c: &Value) -> Result<(), String> {
    k.provide("jobs", JobRegistry::new());
    Ok(())
}

/// session-title 插件：注册配置并启动监听（依赖 agents）
fn start_title(k: &mut Kernel, c: &Value) -> Result<(), String> {
    let cfg = crate::title::TitleConfig {
        fallback_max_words: c.get("fallbackMaxWords").and_then(Value::as_u64).unwrap_or(5) as usize,
        fallback_max_bytes: c.get("fallbackMaxBytes").and_then(Value::as_u64).unwrap_or(40) as usize,
        max_title_bytes: c.get("maxTitleBytes").and_then(Value::as_u64).unwrap_or(80) as usize,
    };
    let agents: Arc<AgentManager> = k.get("agents").ok_or("session-title 需要 agents 服务")?;
    crate::title::start_title_plugin(agents, cfg.clone());
    k.provide("sessionTitle", cfg);
    Ok(())
}

/// agent 插件：组装 AgentManager
fn start_agent(k: &mut Kernel, c: &Value) -> Result<(), String> {
    let llm = k.get::<LlmConfig>("llm").ok_or("agent 需要 llm 服务")?;
    let workdir = k.get::<WorkingDirectory>("workingDirectory")
        .unwrap_or_else(|| Arc::new(WorkingDirectory::new()));
    let bash = k.get::<BashService>("terminals").unwrap_or_else(|| Arc::new(BashService::new()));
    let bash_config = k.get::<crate::bash_tool::BashConfig>("persistentBashCfg")
        .map(|c| *c)
        .unwrap_or_default();
    let retry = k.get::<RetryPolicy>("retry").map(|r| (*r).clone());
    let tools: Arc<ToolsRegistry> = k.get("tools").ok_or("agent 需要 tools 服务")?;
    let session_services: Arc<SessionServices> = k.get("session").ok_or("agent 需要 session 服务")?;
    let sysprompt: Arc<dyn SystemPromptService> = k
        .get::<DefaultSystemPrompt>("systemPrompt")
        .map(|p| p as Arc<dyn SystemPromptService>)
        .ok_or("agent 需要 systemPrompt 服务")?;
    let max_parallel = c.get("maxParallelToolCalls").and_then(Value::as_u64).unwrap_or(10) as usize;
    let mgr = AgentManager {
        agents: std::sync::Mutex::new(std::collections::HashMap::new()),
        bus: k.bus.clone(),
        http: reqwest::Client::new(),
        workdir,
        bash,
        llm_providers: llm.providers(),
        retry,
        session_services,
        sysprompt,
        bash_config,
        tools,
        max_parallel_tool_calls: max_parallel,
    };
    k.provide("agents", mgr);
    Ok(())
}

fn start_jsonrpc(k: &mut Kernel, _c: &Value) -> Result<(), String> {
    k.provide("jsonrpcServer", ());
    Ok(())
}

pub static PLUGIN_DEFS: &[&PluginDef] = &[
    &PluginDef { name: "sdk-app", service: "sdkAppStartup", start: noop },
    &PluginDef { name: "llm", service: "llm", start: start_llm },
    &PluginDef { name: "llm-retry", service: "retry", start: start_retry },
    &PluginDef { name: "session", service: "session", start: start_session },
    &PluginDef { name: "session-projection", service: "sessionProjections", start: start_projection },
    &PluginDef { name: "exterior-dag", service: "dag", start: start_dag },
    &PluginDef { name: "session-persistence-jsonl", service: "sessions", start: start_sessions_jsonl },
    &PluginDef { name: "session-persistence-memory", service: "sessions", start: start_sessions_memory },
    &PluginDef { name: "session-title", service: "sessionTitle", start: start_title },
    &PluginDef { name: "system-prompt", service: "systemPrompt", start: start_sysprompt },
    &PluginDef { name: "working-directory", service: "workingDirectory", start: start_workdir },
    &PluginDef { name: "tool-working-directory", service: "workingDirectoryTool", start: start_tool_workdir },
    &PluginDef { name: "tools", service: "tools", start: start_tools },
    &PluginDef { name: "terminal-bash", service: "terminals", start: start_terminals },
    &PluginDef { name: "persistent-bash", service: "persistentBashCfg", start: start_bash },
    &PluginDef { name: "timer", service: "timer", start: start_timer },
    &PluginDef { name: "jobs", service: "jobs", start: start_jobs },
    &PluginDef { name: "fs-local", service: "fs", start: noop },
    &PluginDef { name: "sandbox-local", service: "sandbox", start: noop },
    &PluginDef { name: "sandbox-policy", service: "sandboxPolicy", start: noop },
    &PluginDef { name: "mcp-resources", service: "mcpResources", start: noop },
    &PluginDef { name: "agent", service: "agents", start: start_agent },
    &PluginDef { name: "agent-loop", service: "agentLoop", start: noop },
    &PluginDef { name: "sdk-jsonrpc-server", service: "jsonrpcServer", start: start_jsonrpc },
];
