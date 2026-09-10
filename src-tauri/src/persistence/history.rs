//! Tauri path adapter for the shared dictation history repository.

use anyhow::Result;
use std::sync::Arc;

use super::{data_dir, ensure_dir};

pub struct HistoryStore {
    inner: Arc<openless_core::HistoryStore>,
}

impl HistoryStore {
    pub fn new() -> Result<Self> {
        let dir = data_dir()?;
        ensure_dir(&dir)?;
        Ok(Self {
            inner: Arc::new(openless_core::HistoryStore::at_data_dir(dir)),
        })
    }

    pub(crate) fn new_fallback() -> Self {
        Self {
            inner: Arc::new(openless_core::HistoryStore::at_path(
                super::fallback_store_path("openless_history_fallback.json"),
            )),
        }
    }

    pub(crate) fn core(&self) -> Arc<openless_core::HistoryStore> {
        Arc::clone(&self.inner)
    }
}
