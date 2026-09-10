//! Shared use-cases for operating on previously captured text or audio.
//!
//! Hosts own file selection and optional foreground-application capture. The
//! core owns immutable provider snapshots, cancellation and attribution.

use std::sync::Arc;

use futures_util::future::BoxFuture;

use crate::config::TaskSpawner;
use crate::dictation_context::{
    DictationContext, DictationProviderInvocations, DictationStartOptions, ProviderInvocation,
};
use crate::errors::{BackendError, BackendErrorCode};
use crate::ports::{TextPolisher, TextStreamChunk, TextStreamSink, TranscriptionEngine};
use crate::types::SessionId;
use crate::{DictionaryStore, PreferencesStore, UserPreferences};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepolishRequest {
    pub raw_text: String,
    pub front_app: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsrCallLabel {
    pub provider: String,
    pub model: Option<String>,
}

impl AsrCallLabel {
    pub fn new(provider: impl Into<String>, model: Option<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.filter(|model| !model.trim().is_empty()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetranscriptionResult {
    pub text: String,
    pub duration_ms: u64,
    pub asr: AsrCallLabel,
}

#[derive(Debug, Clone)]
pub struct RetranscriptionFailure {
    pub error: BackendError,
    pub attempted_asr: Option<AsrCallLabel>,
}

impl RetranscriptionFailure {
    pub fn is_terminal(&self) -> bool {
        !self.error.retryable
    }

    pub fn into_message(self) -> String {
        self.error.message
    }
}

pub trait AuxiliaryApi: Send + Sync {
    fn repolish(
        &self,
        request: RepolishRequest,
    ) -> BoxFuture<'static, Result<String, BackendError>>;

    fn retranscribe_pcm(
        &self,
        pcm: Vec<u8>,
    ) -> BoxFuture<'static, Result<RetranscriptionResult, RetranscriptionFailure>>;
}

pub(crate) struct AuxiliaryService {
    preferences: Arc<PreferencesStore>,
    vocabulary: Arc<DictionaryStore>,
    polisher: Arc<dyn TextPolisher>,
    transcription: Arc<dyn TranscriptionEngine>,
    task_spawner: Arc<dyn TaskSpawner>,
}

impl AuxiliaryService {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        preferences: Arc<PreferencesStore>,
        vocabulary: Arc<DictionaryStore>,
        polisher: Arc<dyn TextPolisher>,
        transcription: Arc<dyn TranscriptionEngine>,
        task_spawner: Arc<dyn TaskSpawner>,
    ) -> Self {
        Self {
            preferences,
            vocabulary,
            polisher,
            transcription,
            task_spawner,
        }
    }

    async fn capture_repolish_context(
        &self,
        front_app: Option<String>,
    ) -> Result<Arc<DictationContext>, BackendError> {
        let preferences = self.preferences.get();
        let llm = ProviderInvocation::for_provider(preferences.active_llm_provider.clone());
        self.capture_context(
            preferences,
            front_app,
            DictationProviderInvocations::new(
                ProviderInvocation::for_provider("auxiliary-unused-asr"),
                llm,
            ),
        )
    }

    async fn capture_retranscription_context(&self) -> Result<Arc<DictationContext>, BackendError> {
        let preferences = self.preferences.get();
        let asr = ProviderInvocation::for_provider(preferences.active_asr_provider.clone());
        self.capture_context(
            preferences,
            None,
            DictationProviderInvocations::new(
                asr,
                ProviderInvocation::for_provider("auxiliary-unused-llm"),
            ),
        )
    }

    fn capture_context(
        &self,
        preferences: UserPreferences,
        front_app: Option<String>,
        providers: DictationProviderInvocations,
    ) -> Result<Arc<DictationContext>, BackendError> {
        let hotwords = self
            .vocabulary
            .list()?
            .into_iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.phrase)
            .collect();
        let options = DictationStartOptions {
            front_app,
            ..DictationStartOptions::default()
        };
        let context =
            DictationContext::capture(&preferences, providers, hotwords, Vec::new(), &options);
        Ok(Arc::new(context))
    }

    fn clone_for_future(&self) -> Self {
        Self {
            preferences: Arc::clone(&self.preferences),
            vocabulary: Arc::clone(&self.vocabulary),
            polisher: Arc::clone(&self.polisher),
            transcription: Arc::clone(&self.transcription),
            task_spawner: Arc::clone(&self.task_spawner),
        }
    }
}

impl AuxiliaryApi for AuxiliaryService {
    fn repolish(
        &self,
        request: RepolishRequest,
    ) -> BoxFuture<'static, Result<String, BackendError>> {
        let service = self.clone_for_future();
        Box::pin(async move {
            let context = service.capture_repolish_context(request.front_app).await?;
            if !context.uses_llm_polisher() {
                return Ok(request.raw_text);
            }
            // 重新润色是一次临时试算：结果不写回历史，诊断快照也随用随弃，
            // 不冒充任何历史记录的原调用。
            let attempt = service
                .polisher
                .polish(
                    SessionId::new(),
                    context,
                    request.raw_text,
                    Arc::new(DiscardTextStream),
                )
                .await;
            Ok(attempt.result?.text)
        })
    }

    fn retranscribe_pcm(
        &self,
        pcm: Vec<u8>,
    ) -> BoxFuture<'static, Result<RetranscriptionResult, RetranscriptionFailure>> {
        let service = self.clone_for_future();
        Box::pin(async move {
            if pcm.is_empty() || !pcm.len().is_multiple_of(2) {
                return Err(RetranscriptionFailure {
                    error: BackendError::new(
                        BackendErrorCode::InvalidArgument,
                        "PCM must contain 16-bit little-endian samples",
                    ),
                    attempted_asr: None,
                });
            }
            let context = service
                .capture_retranscription_context()
                .await
                .map_err(|error| RetranscriptionFailure {
                    error,
                    attempted_asr: None,
                })?;
            let fallback_label =
                AsrCallLabel::new(context.asr.provider_type.clone(), context.asr.model.clone());
            let session_id = SessionId::new();
            let session = service
                .transcription
                .start(session_id, context, Arc::new(DiscardTextStream))
                .await
                .map_err(|mut error| {
                    error.retryable = false;
                    RetranscriptionFailure {
                        error,
                        attempted_asr: Some(fallback_label.clone()),
                    }
                })?;
            let attempted_asr = session
                .asr_call_label()
                .unwrap_or_else(|| fallback_label.clone());
            session.consume_pcm_chunk(&pcm);
            let mut guard = RetranscriptionCancelGuard::new(
                Arc::clone(&session),
                Arc::clone(&service.task_spawner),
            );
            let result = session.finish().await;
            guard.disarm();
            match result {
                Ok(output) => Ok(RetranscriptionResult {
                    text: output.text,
                    duration_ms: output.duration_ms,
                    asr: attempted_asr,
                }),
                Err(mut error) => {
                    error.retryable = false;
                    Err(RetranscriptionFailure {
                        error,
                        attempted_asr: Some(attempted_asr),
                    })
                }
            }
        })
    }
}

struct DiscardTextStream;

impl TextStreamSink for DiscardTextStream {
    fn publish(&self, _chunk: TextStreamChunk) -> Result<(), BackendError> {
        Ok(())
    }
}

struct RetranscriptionCancelGuard {
    session: Option<Arc<dyn crate::ports::TranscriptionSession>>,
    task_spawner: Arc<dyn TaskSpawner>,
}

impl RetranscriptionCancelGuard {
    fn new(
        session: Arc<dyn crate::ports::TranscriptionSession>,
        task_spawner: Arc<dyn TaskSpawner>,
    ) -> Self {
        Self {
            session: Some(session),
            task_spawner,
        }
    }

    fn disarm(&mut self) {
        self.session.take();
    }
}

impl Drop for RetranscriptionCancelGuard {
    fn drop(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        self.task_spawner.spawn(Box::pin(async move {
            let _ = session.cancel().await;
        }));
    }
}

pub(crate) struct UnsupportedAuxiliaryApi;

impl AuxiliaryApi for UnsupportedAuxiliaryApi {
    fn repolish(
        &self,
        _request: RepolishRequest,
    ) -> BoxFuture<'static, Result<String, BackendError>> {
        Box::pin(async {
            Err(BackendError::new(
                BackendErrorCode::Unsupported,
                "auxiliary text processing is not configured",
            ))
        })
    }

    fn retranscribe_pcm(
        &self,
        _pcm: Vec<u8>,
    ) -> BoxFuture<'static, Result<RetranscriptionResult, RetranscriptionFailure>> {
        Box::pin(async {
            Err(RetranscriptionFailure {
                error: BackendError::new(
                    BackendErrorCode::Unsupported,
                    "auxiliary transcription is not configured",
                ),
                attempted_asr: None,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{FixtureTextPolisher, FixtureTranscriptionEngine};
    use crate::TokioTaskSpawner;

    fn service(
        polisher: Arc<dyn TextPolisher>,
        transcription: Arc<dyn TranscriptionEngine>,
    ) -> (AuxiliaryService, std::path::PathBuf) {
        let dictionary_path = std::env::temp_dir().join(format!(
            "openless-core-auxiliary-{}.json",
            uuid::Uuid::new_v4()
        ));
        (
            AuxiliaryService::new(
                Arc::new(PreferencesStore::in_memory()),
                Arc::new(DictionaryStore::at_path(dictionary_path.clone())),
                polisher,
                transcription,
                Arc::new(TokioTaskSpawner),
            ),
            dictionary_path,
        )
    }

    #[tokio::test]
    async fn repolish_always_uses_the_fixed_polisher() {
        let (service, path) = service(
            Arc::new(FixtureTextPolisher::successful("固定润色结果")),
            Arc::new(FixtureTranscriptionEngine::successful("unused", 0)),
        );
        let result = service
            .repolish(RepolishRequest {
                raw_text: "原样输出".into(),
                front_app: Some("Editor".into()),
            })
            .await
            .unwrap();
        assert_eq!(result, "固定润色结果");
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn retranscription_feeds_pcm_and_returns_the_frozen_provider_label() {
        let transcription = Arc::new(FixtureTranscriptionEngine::successful("重转文本", 125));
        let (service, path) = service(
            Arc::new(FixtureTextPolisher::successful("unused")),
            transcription.clone(),
        );
        let expected_provider = service.preferences.get().active_asr_provider;
        let result = service.retranscribe_pcm(vec![1, 0, 2, 0]).await.unwrap();
        assert_eq!(result.text, "重转文本");
        assert_eq!(result.duration_ms, 125);
        assert_eq!(result.asr.provider, expected_provider);
        assert_eq!(transcription.pcm(), vec![1, 0, 2, 0]);
        assert_eq!(transcription.cancel_count(), 0);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn malformed_pcm_is_rejected_before_starting_an_adapter() {
        let transcription = Arc::new(FixtureTranscriptionEngine::successful("unused", 0));
        let (service, path) = service(
            Arc::new(FixtureTextPolisher::successful("unused")),
            transcription.clone(),
        );
        let failure = service.retranscribe_pcm(vec![1]).await.unwrap_err();
        assert_eq!(failure.error.code, BackendErrorCode::InvalidArgument);
        assert!(transcription.pcm().is_empty());
        let _ = std::fs::remove_file(path);
    }
}
