//! Fixed local Qwen3-ASR identifiers shared by Core and the macOS host.

use serde::{Deserialize, Serialize};

use crate::errors::{BackendError, BackendErrorCode};

pub const LOCAL_QWEN3_MLX_PROVIDER_ID: &str = "local-qwen3-mlx";
pub const LOCAL_QWEN3_MODEL_ID: &str = "qwen3-asr-1.7b";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LocalAsrRuntime {
    Qwen3Mlx,
}

impl LocalAsrRuntime {
    pub const fn provider_id(self) -> &'static str {
        LOCAL_QWEN3_MLX_PROVIDER_ID
    }

    pub const fn default_model(self) -> &'static str {
        LOCAL_QWEN3_MODEL_ID
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LocalAsrModelId {
    #[serde(rename = "qwen3-asr-1.7b")]
    Qwen3Asr17b,
}

impl LocalAsrModelId {
    pub const fn as_str(self) -> &'static str {
        LOCAL_QWEN3_MODEL_ID
    }

    pub fn from_wire_id(value: &str) -> Option<Self> {
        (value == LOCAL_QWEN3_MODEL_ID).then_some(Self::Qwen3Asr17b)
    }

    pub const fn all() -> &'static [Self] {
        &[Self::Qwen3Asr17b]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalAsrTarget {
    pub runtime: LocalAsrRuntime,
    model_id: String,
}

impl LocalAsrTarget {
    pub fn fixed() -> Self {
        Self {
            runtime: LocalAsrRuntime::Qwen3Mlx,
            model_id: LOCAL_QWEN3_MODEL_ID.to_string(),
        }
    }

    pub fn parse(
        runtime: LocalAsrRuntime,
        model_id: impl Into<String>,
    ) -> Result<Self, BackendError> {
        let model_id = model_id.into();
        if runtime != LocalAsrRuntime::Qwen3Mlx || model_id != LOCAL_QWEN3_MODEL_ID {
            return Err(BackendError::new(
                BackendErrorCode::InvalidArgument,
                format!("unsupported local ASR target: {model_id}"),
            ));
        }
        Ok(Self { runtime, model_id })
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }
}
