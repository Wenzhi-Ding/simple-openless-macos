use anyhow::Result;
use std::sync::Arc;

pub struct DictionaryStore {
    inner: Arc<openless_core::DictionaryStore>,
}
impl DictionaryStore {
    pub fn new() -> Result<Self> {
        let dir = super::data_dir()?;
        Ok(Self {
            inner: Arc::new(openless_core::DictionaryStore::at_data_dir(dir)),
        })
    }
    pub fn new_fallback() -> Self {
        let path = super::fallback_store_path("dictionary.json");
        Self {
            inner: Arc::new(openless_core::DictionaryStore::at_path(path)),
        }
    }
    pub(crate) fn core(&self) -> Arc<openless_core::DictionaryStore> {
        Arc::clone(&self.inner)
    }
}
