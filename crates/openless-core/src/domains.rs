//! Stable host-facing interfaces for application domains that have platform or
//! transport implementations.
//!
//! The traits in this module are deliberately grouped by use-case instead of
//! mirroring Tauri command names. Hosts only depend on these DTOs and traits;
//! Tauri remains a compatibility adapter for the legacy IPC names.
//!
//! 减法改造(纯本地):云端 provider 校验、QA 追问、Remote Input(LAN 服务端)、
//! Marketplace、Coding Agent、Less Computer、Selection Voice 的接口与 DTO
//! 已随其网络实现整体移除。保留的域全部只依赖本机资源。

use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::errors::{BackendError, BackendErrorCode};

fn unsupported<T>(domain: &'static str) -> BoxFuture<'static, Result<T, BackendError>> {
    Box::pin(async move {
        Err(BackendError::new(
            BackendErrorCode::Unsupported,
            format!("{domain} service is not configured"),
        ))
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalAsrRuntimeStatus {
    pub provider_id: String,
    pub available: bool,
    pub loaded: bool,
    pub active_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    pub keep_loaded_secs: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_prepare_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_transcribe_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_audio_ms: Option<u64>,
}

pub trait LocalAsrApi: Send + Sync {
    fn runtime_status(&self) -> BoxFuture<'static, Result<LocalAsrRuntimeStatus, BackendError>>;
    fn prepare(&self) -> BoxFuture<'static, Result<String, BackendError>>;
    fn cancel_prepare(&self) -> BoxFuture<'static, Result<(), BackendError>>;
    fn release(&self) -> BoxFuture<'static, Result<(), BackendError>>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MicrophoneDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

pub trait PlatformApi: Send + Sync {
    fn capabilities(
        &self,
    ) -> BoxFuture<'static, Result<crate::shared_types::PlatformCapabilities, BackendError>>;
    fn microphone_devices(&self)
        -> BoxFuture<'static, Result<Vec<MicrophoneDevice>, BackendError>>;
    fn microphone_permission(
        &self,
    ) -> BoxFuture<'static, Result<crate::types::PermissionSnapshot, BackendError>>;
    fn accessibility_permission(
        &self,
    ) -> BoxFuture<'static, Result<crate::types::PermissionSnapshot, BackendError>>;
    fn request_microphone_permission(&self) -> BoxFuture<'static, Result<(), BackendError>>;
    fn request_accessibility_permission(&self) -> BoxFuture<'static, Result<(), BackendError>>;
    fn hotkey_status(
        &self,
    ) -> BoxFuture<'static, Result<crate::shared_types::HotkeyStatus, BackendError>>;
}

/// Explicit unsupported adapter used until a host wires a domain implementation.
/// Every call fails with a stable `Unsupported` code; no operation reports fake
/// success and no background task is started.
pub struct UnsupportedDomainServices;

impl LocalAsrApi for UnsupportedDomainServices {
    fn runtime_status(&self) -> BoxFuture<'static, Result<LocalAsrRuntimeStatus, BackendError>> {
        unsupported("local ASR")
    }
    fn prepare(&self) -> BoxFuture<'static, Result<String, BackendError>> {
        unsupported("local ASR")
    }
    fn cancel_prepare(&self) -> BoxFuture<'static, Result<(), BackendError>> {
        unsupported("local ASR")
    }
    fn release(&self) -> BoxFuture<'static, Result<(), BackendError>> {
        unsupported("local ASR")
    }
}

impl PlatformApi for UnsupportedDomainServices {
    fn capabilities(
        &self,
    ) -> BoxFuture<'static, Result<crate::shared_types::PlatformCapabilities, BackendError>> {
        unsupported("platform")
    }
    fn microphone_devices(
        &self,
    ) -> BoxFuture<'static, Result<Vec<MicrophoneDevice>, BackendError>> {
        unsupported("platform")
    }
    fn microphone_permission(
        &self,
    ) -> BoxFuture<'static, Result<crate::types::PermissionSnapshot, BackendError>> {
        unsupported("platform")
    }
    fn accessibility_permission(
        &self,
    ) -> BoxFuture<'static, Result<crate::types::PermissionSnapshot, BackendError>> {
        unsupported("platform")
    }
    fn request_microphone_permission(&self) -> BoxFuture<'static, Result<(), BackendError>> {
        unsupported("platform")
    }
    fn request_accessibility_permission(&self) -> BoxFuture<'static, Result<(), BackendError>> {
        unsupported("platform")
    }
    fn hotkey_status(
        &self,
    ) -> BoxFuture<'static, Result<crate::shared_types::HotkeyStatus, BackendError>> {
        unsupported("platform")
    }
}

#[derive(Clone)]
pub struct BackendServices {
    pub auxiliary: Arc<dyn crate::auxiliary::AuxiliaryApi>,
    pub local_asr: Arc<dyn LocalAsrApi>,
    pub platform: Arc<dyn PlatformApi>,
    pub host_context: Arc<dyn crate::ports::HostContextAdapter>,
    auxiliary_polisher: Option<Arc<dyn crate::ports::TextPolisher>>,
    auxiliary_transcription: Option<Arc<dyn crate::ports::TranscriptionEngine>>,
}

impl BackendServices {
    pub fn unsupported() -> Self {
        Self {
            auxiliary: Arc::new(crate::auxiliary::UnsupportedAuxiliaryApi),
            local_asr: Arc::new(UnsupportedDomainServices),
            platform: Arc::new(UnsupportedDomainServices),
            host_context: Arc::new(crate::ports::NoopHostContextAdapter),
            auxiliary_polisher: None,
            auxiliary_transcription: None,
        }
    }

    /// Configure host-owned provider adapters used by shared auxiliary
    /// use-cases. UI callers use [`Self::auxiliary`], never these adapters.
    #[doc(hidden)]
    pub fn configure_auxiliary_runtime(
        &mut self,
        polisher: Arc<dyn crate::ports::TextPolisher>,
        transcription: Arc<dyn crate::ports::TranscriptionEngine>,
    ) {
        self.auxiliary_polisher = Some(polisher);
        self.auxiliary_transcription = Some(transcription);
    }

    pub(crate) fn take_auxiliary_runtime(
        &mut self,
    ) -> Option<(
        Arc<dyn crate::ports::TextPolisher>,
        Arc<dyn crate::ports::TranscriptionEngine>,
    )> {
        match (
            self.auxiliary_polisher.take(),
            self.auxiliary_transcription.take(),
        ) {
            (Some(polisher), Some(transcription)) => Some((polisher, transcription)),
            (None, None) => None,
            _ => unreachable!("auxiliary runtime adapters are configured atomically"),
        }
    }
}

impl Default for BackendServices {
    fn default() -> Self {
        Self::unsupported()
    }
}

impl std::fmt::Debug for BackendServices {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BackendServices")
            .finish_non_exhaustive()
    }
}
