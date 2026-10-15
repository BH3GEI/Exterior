//! 插件框架（对应 cordis + loader 的 minimal 部分）：
//! 具名服务、声明依赖（inject）、按依赖顺序启动/逆序释放、事件总线、profile 文件决定加载哪些插件。
//! 插件以 Rust 函数形式编译进二进制，不动态加载（SPEC §0.2）。

use crate::session::Event;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tokio::sync::broadcast;

// ---------- 事件总线 ----------

#[derive(Clone, Debug)]
pub enum KernelEvent {
    /// 会话日志新增事件（对应 cordis 的 session/event）
    SessionEvent { session_id: String, event: Event },
    /// agent 状态迁移（对应 agent/status）
    AgentStatus { session_id: String, status: String },
    /// assistant 流式增量（对应 agent/assistant-stream）
    AssistantDelta { session_id: String, text: String },
    /// bash 输出增量（SPEC §3.3：输出边跑边推）
    BashChunk { session_id: String, chunk: String },
}

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<KernelEvent>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(4096);
        EventBus { tx }
    }
    pub fn emit(&self, ev: KernelEvent) {
        let _ = self.tx.send(ev);
    }
    pub fn subscribe(&self) -> broadcast::Receiver<KernelEvent> {
        self.tx.subscribe()
    }
}

// ---------- 插件与 profile ----------

/// profile 文件中的一行（对应 cordis.patch.yml 的 insert 行）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginRow {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub config: Value,
    #[serde(default)]
    pub disabled: bool,
    /// 依赖的具名服务（对应 inject）
    #[serde(default)]
    pub inject: Vec<String>,
}

/// 编译期插件注册表条目
pub struct PluginDef {
    /// 插件名（profile 行的 name；用户行也可用 id 匹配内置行）
    pub name: &'static str,
    /// 本插件提供的具名服务（可为空串表示不提供）
    pub service: &'static str,
    pub start: fn(&mut crate::Kernel, &Value) -> Result<(), String>,
}

pub struct Kernel {
    pub bus: EventBus,
    pub dsh_home: std::path::PathBuf,
    /// 服务名 → 类型擦除实例
    pub services: HashMap<String, std::sync::Arc<dyn std::any::Any + Send + Sync>>,
    /// 启动顺序（逆序释放）
    pub started: Vec<String>,
    pub profile_name: String,
}

impl Kernel {
    pub fn new(profile_name: &str) -> Self {
        Kernel {
            bus: EventBus::new(),
            dsh_home: crate::persist::dsh_home(),
            services: HashMap::new(),
            started: Vec::new(),
            profile_name: profile_name.into(),
        }
    }

    pub fn provide<T: Send + Sync + 'static>(&mut self, name: &str, svc: T) {
        self.services.insert(name.into(), std::sync::Arc::new(svc));
    }

    pub fn get<T: Send + Sync + 'static>(&self, name: &str) -> Option<std::sync::Arc<T>> {
        self.services.get(name)?.clone().downcast::<T>().ok()
    }

    pub fn has(&self, name: &str) -> bool {
        self.services.contains_key(name)
    }
}

/// 合并内置 profile 与用户覆盖（$DSH_HOME/profiles/<name>/cordis.patch.yml）。
/// 用户行按 id 或 name 匹配内置行：覆盖 config/disabled；新行追加（name 必须是编译进来的插件）。
pub fn resolve_profile(builtin: Vec<PluginRow>, profile_name: &str) -> Result<Vec<PluginRow>, String> {
    let path = crate::persist::dsh_home()
        .join("profiles")
        .join(profile_name)
        .join("cordis.patch.yml");
    let mut rows = builtin;
    if !path.exists() {
        return Ok(rows);
    }
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let doc: serde_yaml::Value = serde_yaml::from_str(&text).map_err(|e| format!("profile 解析失败：{e}"))?;
    let mut user_rows: Vec<PluginRow> = Vec::new();
    // 与原版 patch 文件同构："- insert: [...]"
    if let Some(list) = doc.as_sequence() {
        for entry in list {
            if let Some(inserts) = entry.get("insert").and_then(|v| v.as_sequence()) {
                for r in inserts {
                    let j = serde_json::to_value(r).map_err(|e| e.to_string())?;
                    user_rows.push(
                        serde_json::from_value(j)
                            .map_err(|e| format!("profile 行解析失败：{e}"))?,
                    );
                }
            }
        }
    }
    for ur in user_rows {
        if let Some(row) = rows.iter_mut().find(|r| r.id == ur.id || r.name == ur.name) {
            if !ur.config.is_null() {
                // 浅合并 config
                let mut merged = row.config.clone();
                if let (Some(a), Some(b)) = (merged.as_object_mut(), ur.config.as_object()) {
                    for (k, v) in b {
                        a.insert(k.clone(), v.clone());
                    }
                } else {
                    merged = ur.config.clone();
                }
                row.config = merged;
            }
            row.disabled = ur.disabled;
        } else {
            rows.push(ur);
        }
    }
    Ok(rows)
}

/// 按 inject（服务名）拓扑排序后启动启用的插件
pub fn start_plugins(
    kernel: &mut Kernel,
    rows: &[PluginRow],
    defs: &[&'static PluginDef],
) -> Result<(), String> {
    let enabled: Vec<&PluginRow> = rows.iter().filter(|r| !r.disabled).collect();
    let mut pending: Vec<&PluginRow> = enabled.clone();
    let mut provided: Vec<&str> = Vec::new();
    let mut order: Vec<&PluginRow> = Vec::new();
    while !pending.is_empty() {
        let mut progressed = false;
        let mut i = 0;
        while i < pending.len() {
            let row = pending[i];
            let ready = row.inject.iter().all(|s| provided.contains(&s.as_str()));
            if ready {
                let def = defs
                    .iter()
                    .find(|d| d.name == row.name)
                    .ok_or_else(|| format!("未知插件：{}（id {}）", row.name, row.id))?;
                order.push(row);
                if !def.service.is_empty() {
                    provided.push(def.service);
                }
                pending.remove(i);
                progressed = true;
            } else {
                i += 1;
            }
        }
        if !progressed {
            let names: Vec<String> = pending.iter().map(|r| r.id.clone()).collect();
            return Err(format!("插件依赖无法满足：{}", names.join(", ")));
        }
    }
    for row in order {
        let def = defs.iter().find(|d| d.name == row.name).unwrap();
        (def.start)(kernel, &row.config).map_err(|e| format!("插件 {} 启动失败：{e}", row.id))?;
        kernel.started.push(row.id.clone());
    }
    Ok(())
}
