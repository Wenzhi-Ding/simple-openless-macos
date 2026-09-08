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

use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::errors::{BackendError, BackendErrorCode};
use crate::local_asr_catalog::{LocalAsrRuntime, LocalAsrTarget};
use crate::types::{PolishMode, SessionId};

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
pub struct LocalAsrSettings {
    pub runtime: LocalAsrRuntime,
    pub provider_id: String,
    pub active_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models_base_dir: Option<PathBuf>,
    pub models_root_dir: PathBuf,
    pub engine_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_hint: Option<String>,
    pub keep_loaded_secs: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalAsrStorageSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models_base_dir: Option<PathBuf>,
    pub models_root_dir: PathBuf,
    pub is_default: bool,
    #[serde(default)]
    pub restart_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalAsrModel {
    pub target: LocalAsrTarget,
    pub display_name: String,
    pub family: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub languages: Vec<String>,
    pub installed: bool,
    pub downloaded_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalAsrRuntimeStatus {
    pub runtime: LocalAsrRuntime,
    pub provider_id: String,
    pub available: bool,
    pub loaded: bool,
    pub active_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    pub keep_loaded_secs: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalAsrTestResult {
    pub target: LocalAsrTarget,
    pub backend: String,
    pub expected_text: String,
    pub transcribed_text: String,
    pub audio_ms: u64,
    pub load_ms: u64,
    pub transcribe_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalAsrActivationRequest {
    pub target: LocalAsrTarget,
    pub provider_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalAsrActivationResult {
    pub target: LocalAsrTarget,
    pub provider_id: String,
    pub generation: u64,
    pub prepared_model: String,
}

/// 本地 ASR 模型管理接口(纯本地):激活、枚举、预热、测试与删除都在本机
/// 磁盘上完成;上游的远程目录/镜像/下载能力已移除,模型通过本地导入放置。
pub trait LocalAsrApi: Send + Sync {
    fn activate(
        &self,
        _request: LocalAsrActivationRequest,
    ) -> BoxFuture<'static, Result<LocalAsrActivationResult, BackendError>> {
        unsupported("local ASR activation")
    }

    fn settings(
        &self,
        runtime: LocalAsrRuntime,
    ) -> BoxFuture<'static, Result<LocalAsrSettings, BackendError>>;
    fn storage_settings(&self)
        -> BoxFuture<'static, Result<LocalAsrStorageSettings, BackendError>>;
    fn list_models(
        &self,
        runtime: LocalAsrRuntime,
    ) -> BoxFuture<'static, Result<Vec<LocalAsrModel>, BackendError>>;
    fn runtime_status(
        &self,
        runtime: LocalAsrRuntime,
    ) -> BoxFuture<'static, Result<LocalAsrRuntimeStatus, BackendError>>;
    fn set_models_base_dir(
        &self,
        path: Option<PathBuf>,
    ) -> BoxFuture<'static, Result<LocalAsrStorageSettings, BackendError>>;
    fn set_active_model(
        &self,
        target: LocalAsrTarget,
    ) -> BoxFuture<'static, Result<(), BackendError>>;
    fn set_language_hint(
        &self,
        runtime: LocalAsrRuntime,
        language_hint: String,
    ) -> BoxFuture<'static, Result<(), BackendError>>;
    fn set_keep_loaded_secs(
        &self,
        runtime: LocalAsrRuntime,
        seconds: u32,
    ) -> BoxFuture<'static, Result<(), BackendError>>;
    fn prepare(&self, target: LocalAsrTarget) -> BoxFuture<'static, Result<String, BackendError>>;
    fn cancel_prepare(
        &self,
        runtime: LocalAsrRuntime,
    ) -> BoxFuture<'static, Result<(), BackendError>>;
    fn release(&self, runtime: LocalAsrRuntime) -> BoxFuture<'static, Result<(), BackendError>>;
    fn preload(&self, runtime: LocalAsrRuntime) -> BoxFuture<'static, Result<(), BackendError>>;
    fn delete_model(&self, target: LocalAsrTarget) -> BoxFuture<'static, Result<(), BackendError>>;
    fn model_dir(
        &self,
        target: LocalAsrTarget,
    ) -> BoxFuture<'static, Result<PathBuf, BackendError>>;
    fn test_model(
        &self,
        target: LocalAsrTarget,
    ) -> BoxFuture<'static, Result<LocalAsrTestResult, BackendError>>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionPhase {
    #[default]
    Idle,
    Capturing,
    Preview,
    Applying,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionCapture {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_app: Option<String>,
}

/// Platform seam for capturing and replacing the current selection.
///
/// The core owns the session and preview state. Implementations may retain an
/// opaque platform target internally, keyed by `SessionId`, but must never
/// expose native handles through this Interface.
pub trait SelectionRuntimeAdapter: Send + Sync {
    fn capture(
        &self,
        session_id: SessionId,
        supplied_text: Option<String>,
    ) -> BoxFuture<'static, Result<SelectionCapture, BackendError>>;
    fn apply(
        &self,
        session_id: SessionId,
        source_text: String,
        replacement_text: String,
    ) -> BoxFuture<'static, Result<crate::ports::InsertOutcome, BackendError>>;
    fn prepare_preview(
        &self,
        _session_id: SessionId,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        Box::pin(async { Ok(()) })
    }
    fn revert(
        &self,
        session_id: SessionId,
    ) -> BoxFuture<'static, Result<crate::ports::InsertOutcome, BackendError>>;
    fn cancel(&self, session_id: SessionId) -> BoxFuture<'static, Result<(), BackendError>>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionSnapshot {
    pub phase: SelectionPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insert_outcome: Option<crate::ports::InsertOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revert_outcome: Option<crate::ports::InsertOutcome>,
}

impl Default for SelectionSnapshot {
    fn default() -> Self {
        Self {
            phase: SelectionPhase::Idle,
            session_id: None,
            source_text: None,
            preview_text: None,
            instruction: None,
            insert_outcome: None,
            revert_outcome: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionPolishRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_text: Option<String>,
    pub mode: PolishMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
}

pub trait SelectionApi: Send + Sync {
    fn snapshot(&self) -> BoxFuture<'static, Result<SelectionSnapshot, BackendError>>;
    fn begin_polish(
        &self,
        request: SelectionPolishRequest,
    ) -> BoxFuture<'static, Result<SessionId, BackendError>>;
    fn confirm(
        &self,
        session_id: SessionId,
        text: Option<String>,
    ) -> BoxFuture<'static, Result<(), BackendError>>;
    fn cancel(&self, session_id: Option<SessionId>)
        -> BoxFuture<'static, Result<(), BackendError>>;
    fn revert(&self, session_id: SessionId) -> BoxFuture<'static, Result<(), BackendError>>;
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
    fn settings(
        &self,
        _: LocalAsrRuntime,
    ) -> BoxFuture<'static, Result<LocalAsrSettings, BackendError>> {
        unsupported("local ASR")
    }
    fn storage_settings(
        &self,
    ) -> BoxFuture<'static, Result<LocalAsrStorageSettings, BackendError>> {
        unsupported("local ASR")
    }
    fn list_models(
        &self,
        _: LocalAsrRuntime,
    ) -> BoxFuture<'static, Result<Vec<LocalAsrModel>, BackendError>> {
        unsupported("local ASR")
    }
    fn runtime_status(
        &self,
        _: LocalAsrRuntime,
    ) -> BoxFuture<'static, Result<LocalAsrRuntimeStatus, BackendError>> {
        unsupported("local ASR")
    }
    fn set_models_base_dir(
        &self,
        _: Option<PathBuf>,
    ) -> BoxFuture<'static, Result<LocalAsrStorageSettings, BackendError>> {
        unsupported("local ASR")
    }
    fn set_active_model(&self, _: LocalAsrTarget) -> BoxFuture<'static, Result<(), BackendError>> {
        unsupported("local ASR")
    }
    fn set_language_hint(
        &self,
        _: LocalAsrRuntime,
        _: String,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        unsupported("local ASR")
    }
    fn set_keep_loaded_secs(
        &self,
        _: LocalAsrRuntime,
        _: u32,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        unsupported("local ASR")
    }
    fn prepare(&self, _: LocalAsrTarget) -> BoxFuture<'static, Result<String, BackendError>> {
        unsupported("local ASR")
    }
    fn cancel_prepare(&self, _: LocalAsrRuntime) -> BoxFuture<'static, Result<(), BackendError>> {
        unsupported("local ASR")
    }
    fn release(&self, _: LocalAsrRuntime) -> BoxFuture<'static, Result<(), BackendError>> {
        unsupported("local ASR")
    }
    fn preload(&self, _: LocalAsrRuntime) -> BoxFuture<'static, Result<(), BackendError>> {
        unsupported("local ASR")
    }
    fn delete_model(&self, _: LocalAsrTarget) -> BoxFuture<'static, Result<(), BackendError>> {
        unsupported("local ASR")
    }
    fn model_dir(&self, _: LocalAsrTarget) -> BoxFuture<'static, Result<PathBuf, BackendError>> {
        unsupported("local ASR")
    }
    fn test_model(
        &self,
        _: LocalAsrTarget,
    ) -> BoxFuture<'static, Result<LocalAsrTestResult, BackendError>> {
        unsupported("local ASR")
    }
}

impl SelectionApi for UnsupportedDomainServices {
    fn snapshot(&self) -> BoxFuture<'static, Result<SelectionSnapshot, BackendError>> {
        unsupported("selection")
    }
    fn begin_polish(
        &self,
        _: SelectionPolishRequest,
    ) -> BoxFuture<'static, Result<SessionId, BackendError>> {
        unsupported("selection")
    }
    fn confirm(
        &self,
        _: SessionId,
        _: Option<String>,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        unsupported("selection")
    }
    fn cancel(&self, _: Option<SessionId>) -> BoxFuture<'static, Result<(), BackendError>> {
        unsupported("selection")
    }
    fn revert(&self, _: SessionId) -> BoxFuture<'static, Result<(), BackendError>> {
        unsupported("selection")
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
    pub selection: Arc<dyn SelectionApi>,
    pub platform: Arc<dyn PlatformApi>,
    pub host_context: Arc<dyn crate::ports::HostContextAdapter>,
    pub edit_observation: Arc<dyn crate::ports::EditObservationAdapter>,
    auxiliary_polisher: Option<Arc<dyn crate::ports::TextPolisher>>,
    auxiliary_transcription: Option<Arc<dyn crate::ports::TranscriptionEngine>>,
}

impl BackendServices {
    pub fn unsupported() -> Self {
        Self {
            auxiliary: Arc::new(crate::auxiliary::UnsupportedAuxiliaryApi),
            local_asr: Arc::new(UnsupportedDomainServices),
            selection: Arc::new(UnsupportedDomainServices),
            platform: Arc::new(UnsupportedDomainServices),
            host_context: Arc::new(crate::ports::NoopHostContextAdapter),
            edit_observation: Arc::new(crate::ports::NoopEditObservationAdapter),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unsupported_domains_fail_with_a_stable_code() {
        let services = BackendServices::unsupported();
        let error = services
            .local_asr
            .list_models(LocalAsrRuntime::Generic)
            .await
            .unwrap_err();
        assert_eq!(error.code, BackendErrorCode::Unsupported);
        assert!(!error.retryable);
    }
}
