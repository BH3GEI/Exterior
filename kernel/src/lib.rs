//! Exterior 内核：Rust 版 DeepSeek Harness sdk-minimal（SPEC 第 0 节）。

pub mod agent;
pub mod bash_tool;
pub mod cordis;
pub mod jobs;
pub mod llm;
pub mod persist;
pub mod plugins;
pub mod profile;
pub mod server;
pub mod session;
pub mod sysprompt;
pub mod title;
pub mod tools;
pub mod workdir;

pub use cordis::Kernel;

use std::sync::Arc;

/// 启动内核：解析 profile（内置 + 用户覆盖），按依赖顺序启动插件，返回内核。
pub async fn boot(profile_name: &str) -> Result<(Kernel, Arc<agent::AgentManager>), String> {
    let mut kernel = Kernel::new(profile_name);
    let builtin = match profile_name {
        "exterior" => profile::exterior_default(),
        _ => profile::sdk_minimal(),
    };
    let rows = cordis::resolve_profile(builtin, profile_name)?;
    cordis::start_plugins(&mut kernel, &rows, plugins::PLUGIN_DEFS)?;
    let agents: Arc<agent::AgentManager> = kernel
        .get("agents")
        .ok_or("profile 中没有 agent 插件")?;
    Ok((kernel, agents))
}
