//! 本地协作取消原语。
//!
//! 减法改造:CancellationToken 原先住在 coding_agent.rs;Coding Agent 已移除,
//! 但听写引擎 / ports / provider 路由仍共用这个纯同步原语,拆出为独立模块。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct CancellationToken(Arc<AtomicBool>);

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn from_flag(flag: Arc<AtomicBool>) -> Self {
        Self(flag)
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// Wait on the shared flag without blocking process I/O. Hosts and Core use
    /// the same wake-up interval because desktop hotkeys also set this flag
    /// directly; replacing it with a separate notifier would miss those writes.
    pub async fn cancelled(&self) {
        while !self.is_cancelled() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
}
