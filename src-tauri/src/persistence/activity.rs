//! Tauri path adapter for shared daily activity aggregates.

use anyhow::Result;
use std::sync::Arc;

use super::{data_dir, ensure_dir};

pub struct ActivityStore {
    inner: Arc<openless_core::ActivityStore>,
}

impl ActivityStore {
    pub fn load() -> Result<Self> {
        let dir = data_dir()?;
        ensure_dir(&dir)?;
        Ok(Self {
            inner: Arc::new(
                openless_core::ActivityStore::at_data_dir(dir).map_err(anyhow::Error::new)?,
            ),
        })
    }

    pub fn new_fallback() -> Self {
        Self {
            inner: Arc::new(openless_core::ActivityStore::in_memory()),
        }
    }

    pub(crate) fn core(&self) -> Arc<openless_core::ActivityStore> {
        Arc::clone(&self.inner)
    }
}
