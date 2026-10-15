//! 工具注册表（对应 dsh-tools 的最小版）：插件注册 schema，agent 每步动态读取。

use crate::llm::ToolSchema;
use std::sync::Mutex;

pub struct ToolsRegistry {
    schemas: Mutex<Vec<ToolSchema>>,
}

impl ToolsRegistry {
    pub fn new() -> Self {
        ToolsRegistry { schemas: Mutex::new(Vec::new()) }
    }
    pub fn register(&self, schema: ToolSchema) {
        self.schemas.lock().unwrap().push(schema);
    }
    pub fn list(&self) -> Vec<ToolSchema> {
        self.schemas.lock().unwrap().clone()
    }
    pub fn has(&self, name: &str) -> bool {
        self.schemas.lock().unwrap().iter().any(|s| s.name == name)
    }
}
