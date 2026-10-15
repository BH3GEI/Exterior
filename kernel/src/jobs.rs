//! 后台任务与定时器（对应 dsh-jobs-local + cordis-plugin-timer 的最小实现）。
//! minimal profile 中没有模型可见的消费者；提供服务注册与基本行为。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Clone, Debug)]
pub struct JobView {
    pub id: String,
    pub label: String,
    pub status: String, // running | done | error | stopped
}

/// 后台任务登记表
pub struct JobRegistry {
    jobs: Mutex<HashMap<String, JobView>>,
}

impl JobRegistry {
    pub fn new() -> Self {
        JobRegistry { jobs: Mutex::new(HashMap::new()) }
    }
    pub fn start(&self, label: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.jobs.lock().unwrap().insert(
            id.clone(),
            JobView { id: id.clone(), label: label.into(), status: "running".into() },
        );
        id
    }
    pub fn settle(&self, id: &str, ok: bool) {
        if let Some(j) = self.jobs.lock().unwrap().get_mut(id) {
            j.status = if ok { "done".into() } else { "error".into() };
        }
    }
    pub fn list(&self) -> Vec<Value> {
        self.jobs
            .lock()
            .unwrap()
            .values()
            .map(|j| json!({"id": j.id, "label": j.label, "status": j.status}))
            .collect()
    }
}

/// 定时器服务（对应 cordis-plugin-timer 的 setTimeout）
pub struct Timer;

impl Timer {
    pub async fn sleep(ms: u64) {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }
}
