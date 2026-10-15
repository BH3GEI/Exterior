//! 会话投影服务（对应 dsh-session-projection）：从会话日志推导模型可见历史。
//! 默认线性实现；Exterior 用 DAG 实现替换本服务（SPEC §3：不修改内核）。

use crate::llm::Message;
use crate::session::{Event, Session};

/// cordis 服务名 "sessionProjections"
pub trait SessionProjection: Send + Sync {
    /// 事件追加时同步调用（在持久化之后、总线广播之前），保证 derive 读到的是最新状态
    fn observe(&self, _session_id: &str, _session: &Session, _ev: &Event) {}
    /// 推导模型消息历史
    fn derive(&self, session_id: &str, session: &Session) -> Vec<Message>;
}

/// 默认：线性 surface 推导（对应原版行为）
pub struct LinearProjection;

impl SessionProjection for LinearProjection {
    fn derive(&self, _session_id: &str, session: &Session) -> Vec<Message> {
        session.derive_messages()
    }
}
