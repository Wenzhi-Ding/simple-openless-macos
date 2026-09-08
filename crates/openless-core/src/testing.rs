//! Deterministic adapters for host and UI contract tests.
//!
//! These fakes are deliberately small and side-effect free.  They are useful
//! to the Linux egui team when developing a view model without a microphone,
//! network provider, desktop session or credential store.

use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;

use crate::dictation_context::DictationContext;
use crate::domains::{SelectionCapture, SelectionRuntimeAdapter};
use crate::errors::{BackendError, BackendErrorCode};
use crate::shared_types::PlatformCapabilities;
use crate::types::{PermissionSnapshot, PermissionState, PolishDelta, SessionId, TranscriptDelta};
use crate::ports::{
    ActiveRecording, AudioConsumer, AudioRecorder, DictationEngine, EngineFailure, EngineProgress,
    EngineProgressSink, EngineResult, EngineStage, HostAction, HostActions, InsertOutcome,
    InsertWriteResult, RecordingArchive, RecordingProgressSink, TextInserter, TextInsertionSession,
    TextPolisher, TextStreamChunk, TextStreamSink, TranscriptOutput, TranscriptionEngine,
    TranscriptionSession,
};


/// Deterministic selection target used by headless hosts. It can model the
/// Linux contract where capture/direct apply are available while retained
/// preview targets and revert are explicitly unsupported.
#[derive(Clone)]
pub struct FixtureSelectionRuntime {
    capture: SelectionCapture,
    apply_outcome: Result<InsertOutcome, BackendError>,
    prepare_preview: Result<(), BackendError>,
    revert_outcome: Result<InsertOutcome, BackendError>,
    actions: Arc<Mutex<Vec<FixtureSelectionAction>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixtureSelectionAction {
    Capture(SessionId),
    PreparePreview(SessionId),
    Apply(SessionId),
    Revert(SessionId),
    Cancel(SessionId),
}

impl FixtureSelectionRuntime {
    pub fn successful(capture: SelectionCapture, apply_outcome: InsertOutcome) -> Self {
        Self {
            capture,
            apply_outcome: Ok(apply_outcome),
            prepare_preview: Ok(()),
            revert_outcome: Ok(InsertOutcome::Inserted),
            actions: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn linux_preview_unsupported(capture: SelectionCapture) -> Self {
        Self {
            capture,
            apply_outcome: Ok(InsertOutcome::Inserted),
            prepare_preview: Err(BackendError::new(
                BackendErrorCode::Unsupported,
                "Linux selection preview cannot safely retain a target",
            )),
            revert_outcome: Err(BackendError::new(
                BackendErrorCode::Unsupported,
                "Linux selection replacement cannot be safely reverted",
            )),
            actions: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn actions(&self) -> Vec<FixtureSelectionAction> {
        self.actions
            .lock()
            .expect("fixture selection action lock poisoned")
            .clone()
    }
}

impl SelectionRuntimeAdapter for FixtureSelectionRuntime {
    fn capture(
        &self,
        session_id: SessionId,
        supplied_text: Option<String>,
    ) -> BoxFuture<'static, Result<SelectionCapture, BackendError>> {
        self.actions
            .lock()
            .expect("fixture selection action lock poisoned")
            .push(FixtureSelectionAction::Capture(session_id));
        let mut capture = self.capture.clone();
        if let Some(text) = supplied_text {
            capture.text = text;
        }
        Box::pin(async move { Ok(capture) })
    }

    fn apply(
        &self,
        session_id: SessionId,
        _source_text: String,
        _replacement_text: String,
    ) -> BoxFuture<'static, Result<InsertOutcome, BackendError>> {
        self.actions
            .lock()
            .expect("fixture selection action lock poisoned")
            .push(FixtureSelectionAction::Apply(session_id));
        let outcome = self.apply_outcome.clone();
        Box::pin(async move { outcome })
    }

    fn prepare_preview(
        &self,
        session_id: SessionId,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        self.actions
            .lock()
            .expect("fixture selection action lock poisoned")
            .push(FixtureSelectionAction::PreparePreview(session_id));
        let result = self.prepare_preview.clone();
        Box::pin(async move { result })
    }

    fn revert(
        &self,
        session_id: SessionId,
    ) -> BoxFuture<'static, Result<InsertOutcome, BackendError>> {
        self.actions
            .lock()
            .expect("fixture selection action lock poisoned")
            .push(FixtureSelectionAction::Revert(session_id));
        let outcome = self.revert_outcome.clone();
        Box::pin(async move { outcome })
    }

    fn cancel(&self, session_id: SessionId) -> BoxFuture<'static, Result<(), BackendError>> {
        self.actions
            .lock()
            .expect("fixture selection action lock poisoned")
            .push(FixtureSelectionAction::Cancel(session_id));
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug, Clone)]
pub struct FixedClock {
    now_utc: chrono::DateTime<chrono::Utc>,
    today_local: chrono::NaiveDate,
}

impl FixedClock {
    pub fn new(now_utc: chrono::DateTime<chrono::Utc>, today_local: chrono::NaiveDate) -> Self {
        Self {
            now_utc,
            today_local,
        }
    }
}

impl crate::config::Clock for FixedClock {
    fn now_utc(&self) -> chrono::DateTime<chrono::Utc> {
        self.now_utc
    }

    fn today_local(&self) -> chrono::NaiveDate {
        self.today_local
    }
}

#[derive(Clone, Default)]
pub struct RecordingHostActions {
    actions: Arc<Mutex<Vec<HostAction>>>,
}

impl HostActions for RecordingHostActions {
    fn request(&self, action: HostAction) -> Result<(), BackendError> {
        self.actions
            .lock()
            .expect("recording host lock poisoned")
            .push(action);
        Ok(())
    }
}

impl RecordingHostActions {
    pub fn actions(&self) -> Vec<HostAction> {
        self.actions
            .lock()
            .expect("recording host lock poisoned")
            .clone()
    }
}

#[derive(Clone, Default)]
pub struct FixtureAudioRecorder {
    pcm_chunks: Vec<Vec<u8>>,
    levels: Vec<(u64, f32)>,
    stops: Arc<std::sync::atomic::AtomicUsize>,
    has_archived_recording: Option<bool>,
}

impl FixtureAudioRecorder {
    pub fn new(pcm_chunks: Vec<Vec<u8>>, levels: Vec<(u64, f32)>) -> Self {
        Self {
            pcm_chunks,
            levels,
            stops: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            has_archived_recording: None,
        }
    }

    pub fn with_archived_recording(mut self, has_archived_recording: bool) -> Self {
        self.has_archived_recording = Some(has_archived_recording);
        self
    }

    pub fn stop_count(&self) -> usize {
        self.stops.load(std::sync::atomic::Ordering::Acquire)
    }
}

struct FixtureRecordingArchive {
    available: std::sync::atomic::AtomicBool,
    pcm: Vec<u8>,
}

impl RecordingArchive for FixtureRecordingArchive {
    fn is_available(&self) -> bool {
        self.available.load(std::sync::atomic::Ordering::Acquire)
    }

    fn read_pcm(&self) -> BoxFuture<'static, Result<Vec<u8>, BackendError>> {
        let pcm = self.pcm.clone();
        Box::pin(async move { Ok(pcm) })
    }

    fn discard(&self) -> BoxFuture<'static, Result<(), BackendError>> {
        self.available
            .store(false, std::sync::atomic::Ordering::Release);
        Box::pin(async { Ok(()) })
    }
}

struct FixtureActiveRecording {
    stops: Arc<std::sync::atomic::AtomicUsize>,
    archive: Option<Arc<dyn RecordingArchive>>,
}

impl ActiveRecording for FixtureActiveRecording {
    fn archive(&self) -> Option<Arc<dyn RecordingArchive>> {
        self.archive.clone()
    }

    fn stop(self: Box<Self>) -> BoxFuture<'static, Result<(), BackendError>> {
        self.stops.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Box::pin(async { Ok(()) })
    }
}

impl AudioRecorder for FixtureAudioRecorder {
    fn start(
        &self,
        _session_id: SessionId,
        _context: Arc<DictationContext>,
        consumer: Arc<dyn AudioConsumer>,
        progress: Arc<dyn RecordingProgressSink>,
    ) -> BoxFuture<'static, Result<Box<dyn ActiveRecording>, BackendError>> {
        let pcm_chunks = self.pcm_chunks.clone();
        let archived_pcm = pcm_chunks.concat();
        let levels = self.levels.clone();
        let stops = Arc::clone(&self.stops);
        let archive = self.has_archived_recording.map(|available| {
            Arc::new(FixtureRecordingArchive {
                available: std::sync::atomic::AtomicBool::new(available),
                pcm: archived_pcm,
            }) as Arc<dyn RecordingArchive>
        });
        Box::pin(async move {
            for chunk in pcm_chunks {
                consumer.consume_pcm_chunk(&chunk);
            }
            for (elapsed_ms, level) in levels {
                progress.publish_level(elapsed_ms, level)?;
            }
            Ok(Box::new(FixtureActiveRecording { stops, archive }) as Box<dyn ActiveRecording>)
        })
    }
}

#[derive(Clone)]
pub struct FixtureTranscriptionEngine {
    output: Result<TranscriptOutput, BackendError>,
    pcm: Arc<Mutex<Vec<u8>>>,
    cancels: Arc<std::sync::atomic::AtomicUsize>,
}

impl FixtureTranscriptionEngine {
    pub fn successful(text: impl Into<String>, duration_ms: u64) -> Self {
        Self {
            output: Ok(TranscriptOutput {
                text: text.into(),
                duration_ms,
            }),
            pcm: Arc::new(Mutex::new(Vec::new())),
            cancels: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    pub fn failing(error: BackendError) -> Self {
        Self {
            output: Err(error),
            pcm: Arc::new(Mutex::new(Vec::new())),
            cancels: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    pub fn pcm(&self) -> Vec<u8> {
        self.pcm.lock().expect("fixture PCM lock poisoned").clone()
    }

    pub fn cancel_count(&self) -> usize {
        self.cancels.load(std::sync::atomic::Ordering::Acquire)
    }
}

struct FixtureTranscriptionSession {
    output: Result<TranscriptOutput, BackendError>,
    pcm: Arc<Mutex<Vec<u8>>>,
    cancels: Arc<std::sync::atomic::AtomicUsize>,
}

impl AudioConsumer for FixtureTranscriptionSession {
    fn consume_pcm_chunk(&self, pcm: &[u8]) {
        self.pcm
            .lock()
            .expect("fixture PCM lock poisoned")
            .extend_from_slice(pcm);
    }
}

impl TranscriptionSession for FixtureTranscriptionSession {
    fn finish(&self) -> BoxFuture<'static, Result<TranscriptOutput, BackendError>> {
        let output = self.output.clone();
        Box::pin(async move { output })
    }

    fn cancel(&self) -> BoxFuture<'static, Result<(), BackendError>> {
        self.cancels
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Box::pin(async { Ok(()) })
    }
}

impl TranscriptionEngine for FixtureTranscriptionEngine {
    fn start(
        &self,
        _session_id: SessionId,
        _context: Arc<DictationContext>,
        partials: Arc<dyn TextStreamSink>,
    ) -> BoxFuture<'static, Result<Arc<dyn TranscriptionSession>, BackendError>> {
        let output = self.output.clone();
        let pcm = Arc::clone(&self.pcm);
        let cancels = Arc::clone(&self.cancels);
        Box::pin(async move {
            if let Ok(output) = &output {
                partials.publish(TextStreamChunk {
                    text: output.text.clone(),
                    offset: 0,
                })?;
            }
            Ok(Arc::new(FixtureTranscriptionSession {
                output,
                pcm,
                cancels,
            }) as Arc<dyn TranscriptionSession>)
        })
    }
}

#[derive(Clone)]
pub struct FixtureTextPolisher {
    result: Result<crate::ports::PolishOutput, BackendError>,
    cancels: Arc<std::sync::atomic::AtomicUsize>,
    inputs: Arc<Mutex<Vec<String>>>,
}

impl FixtureTextPolisher {
    pub fn successful(text: impl Into<String>) -> Self {
        Self {
            result: Ok(crate::ports::PolishOutput::text(text)),
            cancels: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            inputs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn failing(error: BackendError) -> Self {
        Self {
            result: Err(error),
            cancels: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            inputs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn cancel_count(&self) -> usize {
        self.cancels.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn inputs(&self) -> Vec<String> {
        self.inputs
            .lock()
            .expect("fixture polisher input lock poisoned")
            .clone()
    }
}

impl TextPolisher for FixtureTextPolisher {
    fn polish(
        &self,
        _session_id: SessionId,
        _context: Arc<DictationContext>,
        raw_text: String,
        partials: Arc<dyn TextStreamSink>,
    ) -> BoxFuture<'static, crate::ports::PolishAttempt> {
        let result = self.result.clone();
        self.inputs
            .lock()
            .expect("fixture polisher input lock poisoned")
            .push(raw_text);
        Box::pin(async move {
            match &result {
                Ok(output) => {
                    let published = partials.publish(TextStreamChunk {
                        text: output.text.clone(),
                        offset: 0,
                    });
                    match published {
                        Ok(()) => crate::ports::PolishAttempt::plain(Ok(output.clone())),
                        Err(publish_error) => {
                            crate::ports::PolishAttempt::plain(Err(publish_error))
                        }
                    }
                }
                Err(error) => crate::ports::PolishAttempt::plain(Err(error.clone())),
            }
        })
    }

    fn cancel(&self, _session_id: SessionId) -> BoxFuture<'static, Result<(), BackendError>> {
        self.cancels
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone)]
pub struct FixtureDictationEngine {
    result: Result<EngineResult, EngineFailure>,
    context_update_error: Option<BackendError>,
    polish_deltas: Vec<PolishDelta>,
    actions: Arc<Mutex<Vec<FixtureEngineAction>>>,
    contexts: Arc<Mutex<Vec<Arc<DictationContext>>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureEngineAction {
    Start(SessionId),
    UpdateContext(SessionId),
    FeedAudio(SessionId),
    Finish(SessionId),
    Cancel(SessionId),
}

impl FixtureDictationEngine {
    pub fn successful(raw_text: impl Into<String>, polished_text: impl Into<String>) -> Self {
        Self::successful_with_metadata(raw_text, polished_text, None, 0)
    }

    pub fn successful_with_metadata(
        raw_text: impl Into<String>,
        polished_text: impl Into<String>,
        polish_source: Option<String>,
        duration_ms: u64,
    ) -> Self {
        Self {
            result: Ok(EngineResult {
                raw_text: raw_text.into(),
                asr_transcript: None,
                polished_text: polished_text.into(),
                polish_source,
                duration_ms,
                polish_failed: false,
                asr_ms: None,
                polish_ms: None,
                has_audio_recording: None,
                asr_call_label: None,
                llm_call_label: None,
                polish_diagnostics: None,
            }),
            context_update_error: None,
            polish_deltas: Vec::new(),
            actions: Arc::new(Mutex::new(Vec::new())),
            contexts: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn failing_context_update(
        raw_text: impl Into<String>,
        polished_text: impl Into<String>,
        error: BackendError,
    ) -> Self {
        let mut engine = Self::successful(raw_text, polished_text);
        engine.context_update_error = Some(error);
        engine
    }

    pub fn with_polish_deltas(mut self, deltas: Vec<PolishDelta>) -> Self {
        self.polish_deltas = deltas;
        self
    }

    pub fn failing(error: BackendError) -> Self {
        Self {
            result: Err(EngineFailure::from(error)),
            context_update_error: None,
            polish_deltas: Vec::new(),
            actions: Arc::new(Mutex::new(Vec::new())),
            contexts: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn actions(&self) -> Vec<FixtureEngineAction> {
        self.actions
            .lock()
            .expect("fixture engine lock poisoned")
            .clone()
    }

    pub fn contexts(&self) -> Vec<Arc<DictationContext>> {
        self.contexts
            .lock()
            .expect("fixture engine context lock poisoned")
            .clone()
    }
}

impl DictationEngine for FixtureDictationEngine {
    fn start(
        &self,
        session_id: SessionId,
        context: Arc<DictationContext>,
        _progress: Arc<dyn EngineProgressSink>,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        self.actions
            .lock()
            .expect("fixture engine lock poisoned")
            .push(FixtureEngineAction::Start(session_id));
        self.contexts
            .lock()
            .expect("fixture engine context lock poisoned")
            .push(context);
        Box::pin(async { Ok(()) })
    }

    fn finish(
        &self,
        session_id: SessionId,
        progress: Arc<dyn EngineProgressSink>,
    ) -> BoxFuture<'static, Result<EngineResult, EngineFailure>> {
        self.actions
            .lock()
            .expect("fixture engine lock poisoned")
            .push(FixtureEngineAction::Finish(session_id));
        let result = self.result.clone();
        let polish_deltas = self.polish_deltas.clone();
        Box::pin(async move {
            progress.publish(session_id, EngineProgress::Stage(EngineStage::Transcribing))?;
            let result = result?;
            progress.publish(
                session_id,
                EngineProgress::TranscriptDelta(TranscriptDelta {
                    text: result.raw_text.clone(),
                    offset: 0,
                    is_final: true,
                }),
            )?;
            progress.publish(session_id, EngineProgress::Stage(EngineStage::Polishing))?;
            for delta in polish_deltas {
                progress.publish(session_id, EngineProgress::PolishDelta(delta))?;
            }
            progress.publish(
                session_id,
                EngineProgress::PolishDelta(PolishDelta {
                    text: result.polished_text.clone(),
                    offset: 0,
                    is_final: true,
                }),
            )?;
            Ok(result)
        })
    }

    fn update_context(
        &self,
        session_id: SessionId,
        context: Arc<DictationContext>,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        self.actions
            .lock()
            .expect("fixture engine lock poisoned")
            .push(FixtureEngineAction::UpdateContext(session_id));
        self.contexts
            .lock()
            .expect("fixture engine context lock poisoned")
            .push(context);
        let error = self.context_update_error.clone();
        Box::pin(async move { error.map_or(Ok(()), Err) })
    }

    fn feed_audio(&self, session_id: SessionId, _pcm: &[u8]) -> Result<(), BackendError> {
        self.actions
            .lock()
            .expect("fixture engine lock poisoned")
            .push(FixtureEngineAction::FeedAudio(session_id));
        Ok(())
    }

    fn cancel(&self, session_id: SessionId) -> BoxFuture<'static, Result<(), BackendError>> {
        self.actions
            .lock()
            .expect("fixture engine lock poisoned")
            .push(FixtureEngineAction::Cancel(session_id));
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone)]
pub struct FixtureTextInserter {
    outcome: Result<InsertOutcome, BackendError>,
    actions: Arc<Mutex<Vec<FixtureInsertionAction>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixtureInsertionAction {
    Prepare(SessionId),
    Write { session_id: SessionId, text: String },
    Insert { session_id: SessionId, text: String },
    Copy { session_id: SessionId, text: String },
    Cancel(SessionId),
}

impl FixtureTextInserter {
    pub fn with_outcome(outcome: InsertOutcome) -> Self {
        Self {
            outcome: Ok(outcome),
            actions: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn failing(error: BackendError) -> Self {
        Self {
            outcome: Err(error),
            actions: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn actions(&self) -> Vec<FixtureInsertionAction> {
        self.actions
            .lock()
            .expect("fixture inserter lock poisoned")
            .clone()
    }
}

impl TextInserter for FixtureTextInserter {
    fn begin(
        &self,
        session_id: SessionId,
        _context: Arc<DictationContext>,
    ) -> BoxFuture<'static, Result<Arc<dyn TextInsertionSession>, BackendError>> {
        self.actions
            .lock()
            .expect("fixture inserter lock poisoned")
            .push(FixtureInsertionAction::Prepare(session_id));
        let session = FixtureTextInsertionSession {
            session_id,
            outcome: self.outcome.clone(),
            actions: Arc::clone(&self.actions),
        };
        Box::pin(async move { Ok(Arc::new(session) as Arc<dyn TextInsertionSession>) })
    }
}

#[derive(Clone)]
struct FixtureTextInsertionSession {
    session_id: SessionId,
    outcome: Result<InsertOutcome, BackendError>,
    actions: Arc<Mutex<Vec<FixtureInsertionAction>>>,
}

impl TextInsertionSession for FixtureTextInsertionSession {
    fn write(&self, text: String) -> BoxFuture<'static, Result<InsertWriteResult, BackendError>> {
        let written_chars = text.chars().count();
        self.actions
            .lock()
            .expect("fixture inserter lock poisoned")
            .push(FixtureInsertionAction::Write {
                session_id: self.session_id,
                text,
            });
        Box::pin(async move { Ok(InsertWriteResult { written_chars }) })
    }

    fn copy(&self, text: String) -> BoxFuture<'static, Result<(), BackendError>> {
        self.actions
            .lock()
            .expect("fixture inserter lock poisoned")
            .push(FixtureInsertionAction::Copy {
                session_id: self.session_id,
                text,
            });
        Box::pin(async { Ok(()) })
    }

    fn finish(&self, text: String) -> BoxFuture<'static, Result<InsertOutcome, BackendError>> {
        self.actions
            .lock()
            .expect("fixture inserter lock poisoned")
            .push(FixtureInsertionAction::Insert {
                session_id: self.session_id,
                text,
            });
        let outcome = self.outcome.clone();
        Box::pin(async move { outcome })
    }

    fn cancel(&self) -> BoxFuture<'static, Result<(), BackendError>> {
        self.actions
            .lock()
            .expect("fixture inserter lock poisoned")
            .push(FixtureInsertionAction::Cancel(self.session_id));
        Box::pin(async { Ok(()) })
    }
}

/// Deterministic Linux capability/permission state for view-model and host
/// contract tests.  It describes observable support only; it never probes the
/// machine running the test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxCapabilityFixture {
    pub session: LinuxDesktopSession,
    pub fcitx5_ready: bool,
    pub capabilities: PlatformCapabilities,
    pub permissions: PermissionSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxDesktopSession {
    X11,
    Wayland,
    Headless,
}

impl LinuxCapabilityFixture {
    pub fn x11_full() -> Self {
        Self {
            session: LinuxDesktopSession::X11,
            fcitx5_ready: true,
            capabilities: PlatformCapabilities {
                platform: "linux".to_string(),
                supports_desktop_hotkey: true,
                supports_tray: true,
                supports_overlay: true,
                supports_ime_input: true,
                supports_local_asr: true,
                supports_local_qwen3_mlx: false,
                supports_in_app_dictation: false,
                supports_auto_update: true,
            },
            permissions: PermissionSnapshot {
                microphone: PermissionState::Granted,
                accessibility: PermissionState::Unsupported,
            },
        }
    }

    pub fn wayland_degraded() -> Self {
        Self {
            session: LinuxDesktopSession::Wayland,
            fcitx5_ready: false,
            capabilities: PlatformCapabilities {
                platform: "linux".to_string(),
                supports_desktop_hotkey: false,
                supports_tray: false,
                supports_overlay: false,
                supports_ime_input: false,
                supports_local_asr: true,
                supports_local_qwen3_mlx: false,
                supports_in_app_dictation: false,
                supports_auto_update: false,
            },
            permissions: PermissionSnapshot {
                microphone: PermissionState::Denied,
                accessibility: PermissionState::Unsupported,
            },
        }
    }

    pub fn headless() -> Self {
        Self {
            session: LinuxDesktopSession::Headless,
            fcitx5_ready: false,
            capabilities: PlatformCapabilities {
                platform: "linux".to_string(),
                ..PlatformCapabilities::default()
            },
            permissions: PermissionSnapshot {
                microphone: PermissionState::Unsupported,
                accessibility: PermissionState::Unsupported,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_capability_fixtures_cover_full_degraded_and_headless_hosts() {
        let x11 = LinuxCapabilityFixture::x11_full();
        assert!(x11.fcitx5_ready);
        assert!(x11.capabilities.supports_overlay);

        let wayland = LinuxCapabilityFixture::wayland_degraded();
        assert!(!wayland.fcitx5_ready);
        assert!(!wayland.capabilities.supports_desktop_hotkey);
        assert_eq!(wayland.permissions.microphone, PermissionState::Denied);

        let headless = LinuxCapabilityFixture::headless();
        assert_eq!(headless.session, LinuxDesktopSession::Headless);
        assert_eq!(
            headless.permissions.microphone,
            PermissionState::Unsupported
        );
    }
}