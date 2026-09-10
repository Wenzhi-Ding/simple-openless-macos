//! Session-pinned provider routing shared by every host.
//!
//! Provider/channel settings may change while a dictation is running. Routers
//! therefore resolve an adapter exactly once at session start and keep that
//! adapter alive until the session reaches a terminal state.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use futures_util::future::BoxFuture;

use crate::dictation_context::DictationContext;
use crate::errors::{BackendError, BackendErrorCode};
use crate::ports::{
    DictationEngine, EngineFailure, EngineProgressSink, EngineResult, RecordingProgressSink,
    TextPolisher, TextStreamSink, TranscriptionEngine, TranscriptionSession, VoiceCapture,
};
use crate::types::SessionId;

#[derive(Default)]
pub struct TranscriptionRouter {
    providers: RwLock<HashMap<String, Arc<dyn TranscriptionEngine>>>,
}

impl TranscriptionRouter {
    pub fn register(
        &self,
        provider_id: impl Into<String>,
        provider: Arc<dyn TranscriptionEngine>,
    ) -> Result<Option<Arc<dyn TranscriptionEngine>>, BackendError> {
        let provider_id = normalize_provider_id(provider_id.into())?;
        Ok(self
            .providers
            .write()
            .expect("transcription provider registry lock poisoned")
            .insert(provider_id, provider))
    }

    pub fn remove(&self, provider_id: &str) -> Option<Arc<dyn TranscriptionEngine>> {
        self.providers
            .write()
            .expect("transcription provider registry lock poisoned")
            .remove(provider_id.trim())
    }

    pub fn contains(&self, provider_id: &str) -> bool {
        self.providers
            .read()
            .expect("transcription provider registry lock poisoned")
            .contains_key(provider_id.trim())
    }

    fn resolve(&self, provider_id: &str) -> Result<Arc<dyn TranscriptionEngine>, BackendError> {
        self.providers
            .read()
            .expect("transcription provider registry lock poisoned")
            .get(provider_id.trim())
            .cloned()
            .ok_or_else(|| missing_provider("ASR", provider_id))
    }
}

impl TranscriptionEngine for TranscriptionRouter {
    fn start(
        &self,
        session_id: SessionId,
        context: Arc<DictationContext>,
        partials: Arc<dyn TextStreamSink>,
    ) -> BoxFuture<'static, Result<Arc<dyn TranscriptionSession>, BackendError>> {
        let provider = match self.resolve(&context.asr.provider_type) {
            Ok(provider) => provider,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        provider.start(session_id, context, partials)
    }
}

#[derive(Default)]
pub struct TextPolisherRouter {
    providers: RwLock<HashMap<String, Arc<dyn TextPolisher>>>,
    active: Arc<RwLock<HashMap<SessionId, Arc<dyn TextPolisher>>>>,
}

impl TextPolisherRouter {
    pub fn register(
        &self,
        provider_id: impl Into<String>,
        provider: Arc<dyn TextPolisher>,
    ) -> Result<Option<Arc<dyn TextPolisher>>, BackendError> {
        let provider_id = normalize_provider_id(provider_id.into())?;
        Ok(self
            .providers
            .write()
            .expect("polish provider registry lock poisoned")
            .insert(provider_id, provider))
    }

    pub fn remove(&self, provider_id: &str) -> Option<Arc<dyn TextPolisher>> {
        self.providers
            .write()
            .expect("polish provider registry lock poisoned")
            .remove(provider_id.trim())
    }

    pub fn contains(&self, provider_id: &str) -> bool {
        self.providers
            .read()
            .expect("polish provider registry lock poisoned")
            .contains_key(provider_id.trim())
    }

    fn resolve(&self, provider_id: &str) -> Result<Arc<dyn TextPolisher>, BackendError> {
        self.providers
            .read()
            .expect("polish provider registry lock poisoned")
            .get(provider_id.trim())
            .cloned()
            .ok_or_else(|| missing_provider("LLM", provider_id))
    }
}

impl TextPolisher for TextPolisherRouter {
    fn polish(
        &self,
        session_id: SessionId,
        context: Arc<DictationContext>,
        raw_text: String,
        partials: Arc<dyn TextStreamSink>,
    ) -> BoxFuture<'static, crate::ports::PolishAttempt> {
        let provider = match self.resolve(&context.llm.provider_type) {
            Ok(provider) => provider,
            Err(error) => {
                return Box::pin(async move { crate::ports::PolishAttempt::plain(Err(error)) })
            }
        };
        {
            let mut active = self
                .active
                .write()
                .expect("active polish provider lock poisoned");
            if active.contains_key(&session_id) {
                return Box::pin(async {
                    crate::ports::PolishAttempt::plain(Err(BackendError::new(
                        BackendErrorCode::Busy,
                        "polish provider is already active for this session",
                    )))
                });
            }
            active.insert(session_id, Arc::clone(&provider));
        }
        let registration = ActivePolisherRegistration {
            session_id,
            provider: Arc::clone(&provider),
            active: Arc::clone(&self.active),
        };
        Box::pin(async move {
            let _registration = registration;
            provider
                .polish(session_id, context, raw_text, partials)
                .await
        })
    }

    fn cancel(&self, session_id: SessionId) -> BoxFuture<'static, Result<(), BackendError>> {
        let provider = self
            .active
            .read()
            .expect("active polish provider lock poisoned")
            .get(&session_id)
            .cloned();
        match provider {
            Some(provider) => provider.cancel(session_id),
            None => Box::pin(async { Ok(()) }),
        }
    }
}

struct ActivePolisherRegistration {
    session_id: SessionId,
    provider: Arc<dyn TextPolisher>,
    active: Arc<RwLock<HashMap<SessionId, Arc<dyn TextPolisher>>>>,
}

impl Drop for ActivePolisherRegistration {
    fn drop(&mut self) {
        let mut active = self
            .active
            .write()
            .expect("active polish provider lock poisoned");
        if active
            .get(&self.session_id)
            .is_some_and(|current| Arc::ptr_eq(current, &self.provider))
        {
            active.remove(&self.session_id);
        }
    }
}

/// Pins the fixed ASR + polish pipeline for each active session.
pub struct DictationEngineRouter {
    traditional: Arc<dyn DictationEngine>,
    active: Arc<RwLock<HashMap<SessionId, Arc<RoutedDictationSession>>>>,
}

struct RoutedDictationSession {
    engine: Arc<dyn DictationEngine>,
    started: AtomicBool,
    cancelled: AtomicBool,
}

impl DictationEngineRouter {
    pub fn new(traditional: Arc<dyn DictationEngine>) -> Self {
        Self {
            traditional,
            active: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn resolve(&self) -> Arc<dyn DictationEngine> {
        Arc::clone(&self.traditional)
    }
}

impl DictationEngine for DictationEngineRouter {
    fn start(
        &self,
        session_id: SessionId,
        context: Arc<DictationContext>,
        progress: Arc<dyn EngineProgressSink>,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        let engine = self.resolve();
        let routed = Arc::new(RoutedDictationSession {
            engine: Arc::clone(&engine),
            started: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
        });
        {
            let mut active = self
                .active
                .write()
                .expect("active dictation provider lock poisoned");
            match active.entry(session_id) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(Arc::clone(&routed));
                }
                std::collections::hash_map::Entry::Occupied(_) => {
                    return Box::pin(async {
                        Err(BackendError::new(
                            BackendErrorCode::Busy,
                            "dictation provider is already active for this session",
                        ))
                    });
                }
            }
        }
        let active = Arc::clone(&self.active);
        Box::pin(async move {
            let result = engine.start(session_id, context, progress).await;
            if result.is_err() {
                remove_routed_session(&active, session_id, &routed);
                return result;
            }
            routed.started.store(true, Ordering::Release);
            if routed.cancelled.load(Ordering::Acquire) {
                let cancel_result = engine.cancel(session_id).await;
                remove_routed_session(&active, session_id, &routed);
                cancel_result?;
                return Err(BackendError::new(
                    BackendErrorCode::Cancelled,
                    "dictation was cancelled while its provider was starting",
                ));
            }
            Ok(())
        })
    }

    fn start_transcription(
        &self,
        session_id: SessionId,
        context: Arc<DictationContext>,
        partials: Arc<dyn TextStreamSink>,
    ) -> BoxFuture<'static, Result<Arc<dyn TranscriptionSession>, BackendError>> {
        let engine = self.resolve();
        engine.start_transcription(session_id, context, partials)
    }

    fn start_voice_capture(
        &self,
        session_id: SessionId,
        context: Arc<DictationContext>,
        partials: Arc<dyn TextStreamSink>,
        progress: Arc<dyn RecordingProgressSink>,
        cancel: crate::CancellationToken,
    ) -> BoxFuture<'static, Result<VoiceCapture, BackendError>> {
        let engine = self.resolve();
        engine.start_voice_capture(session_id, context, partials, progress, cancel)
    }

    fn start_audio_capture(
        &self,
        session_id: SessionId,
        context: Arc<DictationContext>,
        progress: Arc<dyn RecordingProgressSink>,
        cancel: crate::CancellationToken,
    ) -> BoxFuture<'static, Result<crate::ports::AudioCapture, BackendError>> {
        self.traditional
            .start_audio_capture(session_id, context, progress, cancel)
    }

    fn finish(
        &self,
        session_id: SessionId,
        progress: Arc<dyn EngineProgressSink>,
    ) -> BoxFuture<'static, Result<EngineResult, EngineFailure>> {
        let routed = self
            .active
            .read()
            .expect("active dictation provider lock poisoned")
            .get(&session_id)
            .cloned();
        let Some(routed) = routed else {
            return Box::pin(async {
                Err(EngineFailure::from(BackendError::new(
                    BackendErrorCode::InvalidState,
                    "dictation provider session is not active",
                )))
            });
        };
        if !routed.started.load(Ordering::Acquire) {
            return Box::pin(async {
                Err(EngineFailure::from(BackendError::new(
                    BackendErrorCode::InvalidState,
                    "dictation provider session is still starting",
                )))
            });
        }
        let engine = Arc::clone(&routed.engine);
        let active = Arc::clone(&self.active);
        Box::pin(async move {
            if routed.cancelled.load(Ordering::Acquire) {
                remove_routed_session(&active, session_id, &routed);
                return Err(EngineFailure::from(BackendError::new(
                    BackendErrorCode::Cancelled,
                    "dictation provider session was cancelled",
                )));
            }
            let result = engine.finish(session_id, progress).await;
            remove_routed_session(&active, session_id, &routed);
            result
        })
    }

    fn update_context(
        &self,
        session_id: SessionId,
        context: Arc<DictationContext>,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        let routed = self
            .active
            .read()
            .expect("active dictation provider lock poisoned")
            .get(&session_id)
            .cloned();
        Box::pin(async move {
            let routed = routed.ok_or_else(|| {
                BackendError::new(
                    BackendErrorCode::InvalidState,
                    "dictation provider session is not active",
                )
            })?;
            if routed.cancelled.load(Ordering::Acquire) {
                return Err(BackendError::new(
                    BackendErrorCode::Cancelled,
                    "dictation provider session was cancelled",
                ));
            }
            routed.engine.update_context(session_id, context).await
        })
    }

    fn feed_audio(&self, session_id: SessionId, pcm: &[u8]) -> Result<(), BackendError> {
        let routed = self
            .active
            .read()
            .expect("active dictation provider lock poisoned")
            .get(&session_id)
            .cloned()
            .ok_or_else(|| {
                BackendError::new(
                    BackendErrorCode::InvalidState,
                    "dictation provider session is not active",
                )
            })?;
        if routed.cancelled.load(Ordering::Acquire) {
            return Err(BackendError::new(
                BackendErrorCode::Cancelled,
                "dictation provider session was cancelled",
            ));
        }
        routed.engine.feed_audio(session_id, pcm)
    }

    fn cancel(&self, session_id: SessionId) -> BoxFuture<'static, Result<(), BackendError>> {
        let routed = self
            .active
            .read()
            .expect("active dictation provider lock poisoned")
            .get(&session_id)
            .cloned();
        let active = Arc::clone(&self.active);
        Box::pin(async move {
            let Some(routed) = routed else {
                return Ok(());
            };
            routed.cancelled.store(true, Ordering::Release);
            let result = routed.engine.cancel(session_id).await;
            if routed.started.load(Ordering::Acquire) {
                remove_routed_session(&active, session_id, &routed);
            }
            result
        })
    }
}

fn remove_routed_session(
    active: &Arc<RwLock<HashMap<SessionId, Arc<RoutedDictationSession>>>>,
    session_id: SessionId,
    expected: &Arc<RoutedDictationSession>,
) {
    let mut active = active
        .write()
        .expect("active dictation provider lock poisoned");
    if active
        .get(&session_id)
        .is_some_and(|current| Arc::ptr_eq(current, expected))
    {
        active.remove(&session_id);
    }
}

fn normalize_provider_id(provider_id: String) -> Result<String, BackendError> {
    let provider_id = provider_id.trim();
    if provider_id.is_empty() {
        return Err(BackendError::new(
            BackendErrorCode::InvalidArgument,
            "provider id must not be blank",
        ));
    }
    Ok(provider_id.to_string())
}

fn missing_provider(kind: &str, provider_id: &str) -> BackendError {
    BackendError::new(
        BackendErrorCode::Unsupported,
        format!("{kind} provider '{}' is not registered", provider_id.trim()),
    )
}

#[cfg(test)]
mod tests {
    use crate::ports::{TextStreamChunk, TranscriptOutput};

    use super::*;

    struct NoopTextSink;

    impl TextStreamSink for NoopTextSink {
        fn publish(&self, _chunk: TextStreamChunk) -> Result<(), BackendError> {
            Ok(())
        }
    }

    struct TaggedTranscriptionEngine(&'static str);

    struct TaggedTranscriptionSession(&'static str);

    impl crate::ports::AudioConsumer for TaggedTranscriptionSession {
        fn consume_pcm_chunk(&self, _pcm: &[u8]) {}
    }

    impl TranscriptionSession for TaggedTranscriptionSession {
        fn finish(&self) -> BoxFuture<'static, Result<TranscriptOutput, BackendError>> {
            let tag = self.0.to_string();
            Box::pin(async move {
                Ok(TranscriptOutput {
                    text: tag,
                    duration_ms: 1,
                })
            })
        }

        fn cancel(&self) -> BoxFuture<'static, Result<(), BackendError>> {
            Box::pin(async { Ok(()) })
        }
    }

    impl TranscriptionEngine for TaggedTranscriptionEngine {
        fn start(
            &self,
            _session_id: SessionId,
            _context: Arc<DictationContext>,
            _partials: Arc<dyn TextStreamSink>,
        ) -> BoxFuture<'static, Result<Arc<dyn TranscriptionSession>, BackendError>> {
            let tag = self.0;
            Box::pin(async move {
                Ok(Arc::new(TaggedTranscriptionSession(tag)) as Arc<dyn TranscriptionSession>)
            })
        }
    }

    struct TaggedPolisher(&'static str);

    impl TextPolisher for TaggedPolisher {
        fn polish(
            &self,
            _session_id: SessionId,
            _context: Arc<DictationContext>,
            raw_text: String,
            _partials: Arc<dyn TextStreamSink>,
        ) -> BoxFuture<'static, crate::ports::PolishAttempt> {
            let tag = self.0;
            Box::pin(async move {
                crate::ports::PolishAttempt::plain(Ok(crate::ports::PolishOutput::text(format!(
                    "{tag}:{raw_text}"
                ))))
            })
        }

        fn cancel(&self, _session_id: SessionId) -> BoxFuture<'static, Result<(), BackendError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[allow(clippy::field_reassign_with_default)]
    fn context_with_providers(asr: &str, llm: &str) -> DictationContext {
        let mut context = DictationContext::default();
        context.asr.provider_id = asr.to_string();
        context.asr.provider_type = asr.to_string();
        context.llm.provider_id = llm.to_string();
        context.llm.provider_type = llm.to_string();
        context
    }

    #[tokio::test]
    async fn asr_and_llm_routes_follow_the_captured_provider_ids() {
        let transcription = TranscriptionRouter::default();
        transcription
            .register("asr-a", Arc::new(TaggedTranscriptionEngine("asr-a")))
            .unwrap();
        let polisher = TextPolisherRouter::default();
        polisher
            .register("llm-a", Arc::new(TaggedPolisher("llm-a")))
            .unwrap();
        let context = Arc::new(context_with_providers("asr-a", "llm-a"));
        let session_id = SessionId::new();

        let session = transcription
            .start(session_id, Arc::clone(&context), Arc::new(NoopTextSink))
            .await
            .unwrap();
        assert_eq!(session.finish().await.unwrap().text, "asr-a");
        assert_eq!(
            polisher
                .polish(
                    session_id,
                    context,
                    "raw".to_string(),
                    Arc::new(NoopTextSink),
                )
                .await
                .result
                .unwrap()
                .text,
            "llm-a:raw"
        );
    }

    #[tokio::test]
    async fn routes_by_protocol_type_while_preserving_the_channel_identity() {
        let transcription = TranscriptionRouter::default();
        transcription
            .register(
                "asr-protocol",
                Arc::new(TaggedTranscriptionEngine("asr-protocol")),
            )
            .unwrap();
        let polisher = TextPolisherRouter::default();
        polisher
            .register("llm-protocol", Arc::new(TaggedPolisher("llm-protocol")))
            .unwrap();
        let mut context = context_with_providers("asr-channel", "llm-channel");
        context.asr.provider_type = "asr-protocol".to_string();
        context.llm.provider_type = "llm-protocol".to_string();
        let context = Arc::new(context);
        let session_id = SessionId::new();

        let session = transcription
            .start(session_id, Arc::clone(&context), Arc::new(NoopTextSink))
            .await
            .unwrap();
        assert_eq!(session.finish().await.unwrap().text, "asr-protocol");
        assert_eq!(context.asr.provider_id, "asr-channel");
        assert_eq!(
            polisher
                .polish(
                    session_id,
                    Arc::clone(&context),
                    "raw".to_string(),
                    Arc::new(NoopTextSink),
                )
                .await
                .result
                .unwrap()
                .text,
            "llm-protocol:raw"
        );
        assert_eq!(context.llm.provider_id, "llm-channel");
    }

    #[tokio::test]
    async fn missing_selected_provider_is_an_explicit_unsupported_error() {
        let router = TranscriptionRouter::default();
        let context = context_with_providers("missing", "llm");
        let error = router
            .start(SessionId::new(), Arc::new(context), Arc::new(NoopTextSink))
            .await
            .err()
            .expect("missing provider must fail");
        assert_eq!(error.code, BackendErrorCode::Unsupported);
    }
}
