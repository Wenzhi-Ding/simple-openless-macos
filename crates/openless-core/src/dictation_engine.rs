//! Shared dictation pipeline orchestration.
//!
//! The pipeline owns provider/recorder ordering, terminal progress events and
//! cancellation guards. Native hosts only implement the narrow ports from
//! [`crate::ports`]; they never duplicate session or fallback decisions.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use futures_util::future::BoxFuture;

use crate::dictation_context::DictationContext;
use crate::errors::{BackendError, BackendErrorCode};
use crate::polish_diagnostics::PolishDiagnostics;
use crate::ports::{
    ActiveRecording, AudioCapture, AudioConsumer, AudioRecorder, CapturedPcm, DictationEngine,
    EngineFailure, EngineFailureStage, EngineProgress, EngineProgressSink, EngineResult,
    EngineStage, PolishAttempt, RecordingProgressSink, TextPolisher, TextStreamChunk,
    TextStreamSink, TranscriptionEngine, TranscriptionSession, VoiceCapture,
};
use crate::types::{SessionId, TranscriptDelta};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolishFailurePolicy {
    Fail,
    UseRawText,
}

pub struct PipelineDictationEngine {
    recorder: Arc<dyn AudioRecorder>,
    transcription: Arc<dyn TranscriptionEngine>,
    polisher: Arc<dyn TextPolisher>,
    polish_failure_policy: PolishFailurePolicy,
    sessions: Arc<Mutex<HashMap<SessionId, Arc<PipelineSession>>>>,
}

struct PipelineSession {
    context: RwLock<Arc<DictationContext>>,
    cancelled: AtomicBool,
    finishing: AtomicBool,
    transcription_finished: AtomicBool,
    transcription_cancelled: AtomicBool,
    polishing: AtomicBool,
    polisher_cancelled: AtomicBool,
    recording_fault: Mutex<Option<BackendError>>,
    /// 采集期间是否出现过非零 PCM 样本。finalize 用它区分「麦克风没出数据」
    /// 和「安静但有效的录音」：前者显式报错，后者照常交给 ASR/VAD。
    pcm_signal: Arc<AudioSignalProbe>,
    resources: Mutex<PipelineResources>,
}

/// 单次会话的 PCM 观测：只在首次看到非零字节时写一次原子量。
#[derive(Default)]
struct AudioSignalProbe {
    has_nonzero_pcm: AtomicBool,
}

impl AudioSignalProbe {
    /// 任意一个字节非零即视为设备出过数据；全零（或零长度）不算。
    fn observe(&self, pcm: &[u8]) {
        if !self.has_nonzero_pcm.load(Ordering::Relaxed) && pcm.iter().any(|byte| *byte != 0) {
            self.has_nonzero_pcm.store(true, Ordering::Release);
        }
    }

    fn has_nonzero_pcm(&self) -> bool {
        self.has_nonzero_pcm.load(Ordering::Acquire)
    }
}

#[derive(Default)]
struct PipelineResources {
    recording: Option<Box<dyn ActiveRecording>>,
    transcription: Option<Arc<dyn TranscriptionSession>>,
}

impl PipelineSession {
    fn new(context: Arc<DictationContext>) -> Self {
        Self {
            context: RwLock::new(context),
            cancelled: AtomicBool::new(false),
            finishing: AtomicBool::new(false),
            transcription_finished: AtomicBool::new(false),
            transcription_cancelled: AtomicBool::new(false),
            polishing: AtomicBool::new(false),
            polisher_cancelled: AtomicBool::new(false),
            recording_fault: Mutex::new(None),
            pcm_signal: Arc::new(AudioSignalProbe::default()),
            resources: Mutex::new(PipelineResources::default()),
        }
    }

    fn context(&self) -> Arc<DictationContext> {
        Arc::clone(&self.context.read().expect("pipeline context lock poisoned"))
    }

    fn update_context(&self, context: Arc<DictationContext>) -> Result<(), BackendError> {
        if self.finishing.load(Ordering::Acquire) || self.cancelled.load(Ordering::Acquire) {
            return Err(BackendError::new(
                BackendErrorCode::InvalidState,
                "dictation pipeline context can only change before finalization",
            ));
        }
        *self
            .context
            .write()
            .expect("pipeline context lock poisoned") = context;
        Ok(())
    }
}

impl PipelineDictationEngine {
    pub fn new(
        recorder: Arc<dyn AudioRecorder>,
        transcription: Arc<dyn TranscriptionEngine>,
        polisher: Arc<dyn TextPolisher>,
    ) -> Self {
        Self {
            recorder,
            transcription,
            polisher,
            polish_failure_policy: PolishFailurePolicy::UseRawText,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn with_polish_failure_policy(mut self, policy: PolishFailurePolicy) -> Self {
        self.polish_failure_policy = policy;
        self
    }
}

impl DictationEngine for PipelineDictationEngine {
    fn start(
        &self,
        session_id: SessionId,
        context: Arc<DictationContext>,
        progress: Arc<dyn EngineProgressSink>,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        let recorder = Arc::clone(&self.recorder);
        let transcription_engine = Arc::clone(&self.transcription);
        let sessions = Arc::clone(&self.sessions);
        Box::pin(async move {
            let session = Arc::new(PipelineSession::new(Arc::clone(&context)));
            {
                let mut active = sessions.lock().expect("pipeline session lock poisoned");
                match active.entry(session_id) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(Arc::clone(&session));
                    }
                    std::collections::hash_map::Entry::Occupied(_) => {
                        return Err(BackendError::new(
                            BackendErrorCode::Busy,
                            "dictation pipeline session already exists",
                        ));
                    }
                }
            }

            let transcript_partials: Arc<dyn TextStreamSink> =
                Arc::new(TranscriptProgressForwarder {
                    session_id,
                    progress: Arc::clone(&progress),
                });
            let transcription = match transcription_engine
                .start(session_id, Arc::clone(&context), transcript_partials)
                .await
            {
                Ok(transcription) => transcription,
                Err(error) => {
                    remove_session(&sessions, session_id, &session);
                    return Err(error);
                }
            };

            {
                let mut resources = session
                    .resources
                    .lock()
                    .expect("pipeline resource lock poisoned");
                if !session.cancelled.load(Ordering::Acquire) {
                    resources.transcription = Some(Arc::clone(&transcription));
                }
            }
            if session.cancelled.load(Ordering::Acquire) {
                let cancel_result =
                    cancel_transcription_once(&session, Arc::clone(&transcription)).await;
                remove_session(&sessions, session_id, &session);
                cancel_result?;
                return Err(cancelled_error(
                    "dictation cancelled while ASR was starting",
                ));
            }

            let audio_consumer: Arc<dyn AudioConsumer> = Arc::new(SessionAudioConsumer {
                session: Arc::clone(&transcription),
                pcm_signal: Arc::clone(&session.pcm_signal),
            });
            let recording_progress: Arc<dyn RecordingProgressSink> =
                Arc::new(RecordingProgressForwarder {
                    session_id,
                    session: Arc::clone(&session),
                    progress,
                });
            let recording = match recorder
                .start(session_id, context, audio_consumer, recording_progress)
                .await
            {
                Ok(recording) => recording,
                Err(error) => {
                    let _ = cancel_transcription_once(&session, transcription).await;
                    remove_session(&sessions, session_id, &session);
                    return Err(error);
                }
            };

            let mut recording = Some(recording);
            {
                let mut resources = session
                    .resources
                    .lock()
                    .expect("pipeline resource lock poisoned");
                if !session.cancelled.load(Ordering::Acquire) {
                    resources.recording = recording.take();
                }
            }
            if let Some(recording) = recording {
                let stop_result = recording.stop().await;
                let cancel_result = cancel_transcription_once(&session, transcription).await;
                remove_session(&sessions, session_id, &session);
                stop_result?;
                cancel_result?;
                return Err(cancelled_error(
                    "dictation cancelled while the recorder was starting",
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
        self.transcription.start(session_id, context, partials)
    }

    fn start_voice_capture(
        &self,
        session_id: SessionId,
        context: Arc<DictationContext>,
        partials: Arc<dyn TextStreamSink>,
        progress: Arc<dyn RecordingProgressSink>,
        cancel: crate::CancellationToken,
    ) -> BoxFuture<'static, Result<VoiceCapture, BackendError>> {
        let recorder = Arc::clone(&self.recorder);
        let transcription_engine = Arc::clone(&self.transcription);
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(cancelled_error(
                    "voice capture cancelled before ASR startup",
                ));
            }
            let transcription = transcription_engine
                .start(session_id, Arc::clone(&context), partials)
                .await?;
            if cancel.is_cancelled() {
                let _ = transcription.cancel().await;
                return Err(cancelled_error(
                    "voice capture cancelled while ASR was starting",
                ));
            }
            let consumer: Arc<dyn AudioConsumer> = Arc::new(SessionAudioConsumer {
                session: Arc::clone(&transcription),
                pcm_signal: Arc::new(AudioSignalProbe::default()),
            });
            match recorder
                .start(session_id, context, consumer, progress)
                .await
            {
                Ok(recording) if cancel.is_cancelled() => {
                    let _ = recording.stop().await;
                    let _ = transcription.cancel().await;
                    Err(cancelled_error(
                        "voice capture cancelled while recorder was starting",
                    ))
                }
                Ok(recording) => Ok(VoiceCapture {
                    recording,
                    transcription,
                }),
                Err(error) => {
                    let _ = transcription.cancel().await;
                    Err(error)
                }
            }
        })
    }

    fn start_audio_capture(
        &self,
        session_id: SessionId,
        context: Arc<DictationContext>,
        progress: Arc<dyn RecordingProgressSink>,
        cancel: crate::CancellationToken,
    ) -> BoxFuture<'static, Result<AudioCapture, BackendError>> {
        let recorder = Arc::clone(&self.recorder);
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(cancelled_error(
                    "voice capture cancelled before recorder startup",
                ));
            }
            let pcm = Arc::new(CapturedPcm::default());
            let consumer: Arc<dyn AudioConsumer> = pcm.clone();
            let recording = recorder
                .start(session_id, context, consumer, progress)
                .await?;
            if cancel.is_cancelled() {
                let _ = recording.stop().await;
                return Err(cancelled_error(
                    "voice capture cancelled while recorder was starting",
                ));
            }
            Ok(AudioCapture { recording, pcm })
        })
    }

    fn finish(
        &self,
        session_id: SessionId,
        progress: Arc<dyn EngineProgressSink>,
    ) -> BoxFuture<'static, Result<EngineResult, EngineFailure>> {
        let sessions = Arc::clone(&self.sessions);
        let transcription_engine = Arc::clone(&self.transcription);
        let polisher = Arc::clone(&self.polisher);
        let policy = self.polish_failure_policy;
        Box::pin(async move {
            let session = find_session(&sessions, session_id)?;
            if session.finishing.swap(true, Ordering::AcqRel) {
                return Err(BackendError::new(
                    BackendErrorCode::Busy,
                    "dictation pipeline is already finishing",
                )
                .into());
            }
            if session.cancelled.load(Ordering::Acquire) {
                remove_session(&sessions, session_id, &session);
                return Err(cancelled_error("dictation was cancelled before finishing").into());
            }
            let context = session.context();

            let (recording, transcription) = {
                let mut resources = session
                    .resources
                    .lock()
                    .expect("pipeline resource lock poisoned");
                (resources.recording.take(), resources.transcription.clone())
            };
            let recording = recording.ok_or_else(|| {
                BackendError::new(
                    BackendErrorCode::InvalidState,
                    "dictation recorder has not started",
                )
            })?;
            let transcription = transcription.ok_or_else(|| {
                BackendError::new(
                    BackendErrorCode::InvalidState,
                    "transcription session has not started",
                )
            })?;

            let archive = recording.archive();
            let mut has_audio_recording = archive.as_ref().map(|archive| archive.is_available());
            if let Err(error) = recording.stop().await {
                let _ = cancel_transcription_once(&session, transcription).await;
                remove_session(&sessions, session_id, &session);
                let mut failure = EngineFailure::new(error, EngineFailureStage::Transcribing);
                failure.has_audio_recording = has_audio_recording;
                return Err(failure);
            }
            if session.cancelled.load(Ordering::Acquire) {
                let _ = cancel_transcription_once(&session, transcription).await;
                remove_session(&sessions, session_id, &session);
                return Err(cancelled_error(
                    "dictation was cancelled before transcription finished",
                )
                .into());
            }
            // 采集已停止，这里看到的就是整段录音：全零（或零长度）说明
            // 麦克风根本没出数据（设备被拔掉、被系统静音成数字零、流没真正
            // 启动），显式报错让用户去查输入设备。安静但非零的样本是有效
            // 录音，不算 —— 那种情况照旧交给 ASR/VAD 走空转写（emptyTranscript）。
            if !session.pcm_signal.has_nonzero_pcm() {
                let _ = cancel_transcription_once(&session, transcription).await;
                remove_session(&sessions, session_id, &session);
                let mut failure = EngineFailure::new(
                    BackendError::new(
                        BackendErrorCode::NoAudio,
                        "未收到有效音频，请检查麦克风输入设备后重试",
                    ),
                    EngineFailureStage::Transcribing,
                );
                failure.has_audio_recording = has_audio_recording;
                return Err(failure);
            }

            publish_progress(
                &session,
                session_id,
                &progress,
                EngineProgress::Stage(EngineStage::Transcribing),
            )?;
            let asr_started = std::time::Instant::now();
            let mut asr_call_label = transcription.asr_call_label();
            let transcription_result = transcription.finish().await;
            for notification in transcription.take_progress_notifications() {
                publish_progress(
                    &session,
                    session_id,
                    &progress,
                    EngineProgress::Notification(notification),
                )?;
            }
            let transcript = match transcription_result {
                Ok(transcript) => {
                    session
                        .transcription_finished
                        .store(true, Ordering::Release);
                    transcript
                }
                Err(first_error) => {
                    let cancelled = session.cancelled.load(Ordering::Acquire);
                    let retry_pcm = if !cancelled && first_error.retryable {
                        match archive.as_ref().filter(|archive| archive.is_available()) {
                            Some(archive) => {
                                archive.read_pcm().await.ok().filter(|pcm| !pcm.is_empty())
                            }
                            None => None,
                        }
                    } else {
                        None
                    };
                    let _ = cancel_transcription_once(&session, Arc::clone(&transcription)).await;
                    match retry_pcm {
                        Some(pcm) => match retry_transcription(
                            transcription_engine,
                            Arc::clone(&session),
                            session_id,
                            Arc::clone(&context),
                            Arc::clone(&progress),
                            pcm,
                        )
                        .await
                        {
                            Ok((transcript, label)) => {
                                asr_call_label = label;
                                transcript
                            }
                            Err((error, label)) => {
                                asr_call_label = label.or(asr_call_label);
                                remove_session(&sessions, session_id, &session);
                                let mut failure =
                                    EngineFailure::new(error, EngineFailureStage::Transcribing);
                                failure.asr_ms = Some(asr_started.elapsed().as_millis() as u64);
                                failure.has_audio_recording = has_audio_recording;
                                failure.asr_call_label = asr_call_label;
                                return Err(failure);
                            }
                        },
                        None => {
                            remove_session(&sessions, session_id, &session);
                            let error = if cancelled {
                                cancelled_error(
                                    "dictation was cancelled while transcription was finishing",
                                )
                            } else {
                                first_error
                            };
                            let mut failure =
                                EngineFailure::new(error, EngineFailureStage::Transcribing);
                            failure.asr_ms = Some(asr_started.elapsed().as_millis() as u64);
                            failure.has_audio_recording = has_audio_recording;
                            failure.asr_call_label = asr_call_label;
                            failure.polish_diagnostics =
                                Some(PolishDiagnostics::not_called_from_context(
                                    &context,
                                    "transcription failed before the polish model was called",
                                ));
                            return Err(failure);
                        }
                    }
                }
            };
            let asr_ms = Some(asr_started.elapsed().as_millis() as u64);
            if session.cancelled.load(Ordering::Acquire) {
                remove_session(&sessions, session_id, &session);
                return Err(cancelled_error(
                    "dictation was cancelled after transcription finished",
                )
                .into());
            }
            let original_asr_text = transcript.text.clone();
            let asr_transcript =
                (transcript.text != original_asr_text).then_some(original_asr_text);
            if !context.recording.archive_successful_recording && !transcript.text.trim().is_empty()
            {
                if let Some(archive) = archive.as_ref() {
                    if archive.is_available() {
                        let _ = archive.discard().await;
                    }
                    has_audio_recording = Some(archive.is_available());
                }
            }
            publish_progress(
                &session,
                session_id,
                &progress,
                EngineProgress::TranscriptDelta(TranscriptDelta {
                    text: transcript.text.clone(),
                    offset: 0,
                    is_final: true,
                }),
            )?;
            // 短输入跳润色（用户 2026-11-02 批准，阈值见 POLISH_SKIP_MAX_CHARS）：
            // 词表同音纠正已在转写适配器完成，这里拿到的 transcript.text 就是
            // 本来要送进 Gemma 的那份输入；非空白字符数不足阈值时直接按 raw 插入。
            let skips_for_short_text = context.uses_llm_polisher()
                && context.skips_polish_for_short_text(&transcript.text);
            let uses_polisher = context.uses_llm_polisher() && !skips_for_short_text;
            // 屏幕 OCR 结果在润色输入冻结的这一刻非阻塞收取：未就绪即忽略，
            // 绝不为它等待（转写完成时仍未就绪的识别就此作废）。
            let context = if uses_polisher && context.has_pending_screen_ocr() {
                Arc::new(context.with_screen_ocr_frozen())
            } else {
                context
            };
            let (polish_output, polish_failed, polish_ms, polish_diagnostics) = if uses_polisher {
                publish_progress(
                    &session,
                    session_id,
                    &progress,
                    EngineProgress::Stage(EngineStage::Polishing),
                )?;

                session.polishing.store(true, Ordering::Release);
                if session.cancelled.load(Ordering::Acquire) {
                    let _ = cancel_polisher_once(&session, &polisher, session_id).await;
                    remove_session(&sessions, session_id, &session);
                    return Err(cancelled_error(
                        "dictation was cancelled before polishing started",
                    )
                    .into());
                }
                // 流式上屏已删除：润色仍走 SSE 流式生成，但增量不再向上转发，
                // 只有最终文本进入 EngineResult 走一次性插入。
                let polish_partials: Arc<dyn TextStreamSink> = Arc::new(NoopTextStreamSink);
                let polish_started = std::time::Instant::now();
                let attempt: PolishAttempt = if let Some(error) = context.deferred_llm_error.clone()
                {
                    // 启动时就解析失败（如 LLM provider 不可用）：模型没被调用，
                    // 快照记 NotCalled 而不是空。
                    PolishAttempt {
                        result: Err(error),
                        diagnostics: Some(PolishDiagnostics::not_called_from_context(
                            &context,
                            "llm provider was unavailable when the session started",
                        )),
                    }
                } else {
                    polisher
                        .polish(
                            session_id,
                            context,
                            transcript.text.clone(),
                            polish_partials,
                        )
                        .await
                };
                // 诊断快照与结果解耦：成功、回退 raw、失败都带同一份快照
                // （回退 raw 的快照是失败尝试的真实 prompt）。
                let polish_diagnostics = attempt.diagnostics;
                let result = match attempt.result {
                    Ok(text) => (text, false),
                    Err(error) if can_fallback_to_raw(policy, &error) => (
                        crate::ports::PolishOutput::text(transcript.text.clone()),
                        true,
                    ),
                    Err(error) => {
                        let polish_ms = Some(polish_started.elapsed().as_millis() as u64);
                        let cancelled = session.cancelled.load(Ordering::Acquire);
                        let _ = cancel_polisher_once(&session, &polisher, session_id).await;
                        remove_session(&sessions, session_id, &session);
                        let error = if cancelled {
                            cancelled_error("dictation was cancelled while polishing was running")
                        } else {
                            error
                        };
                        let mut failure = EngineFailure::new(error, EngineFailureStage::Polishing);
                        failure.raw_text = Some(transcript.text.clone());
                        failure.duration_ms = Some(transcript.duration_ms);
                        failure.asr_ms = asr_ms;
                        failure.polish_ms = polish_ms;
                        failure.has_audio_recording = has_audio_recording;
                        failure.asr_call_label = asr_call_label.clone();
                        failure.polish_diagnostics = polish_diagnostics;
                        return Err(failure);
                    }
                };
                let polish_ms = Some(polish_started.elapsed().as_millis() as u64);
                session.polishing.store(false, Ordering::Release);
                (result.0, result.1, polish_ms, polish_diagnostics)
            } else {
                // Raw 直通：润色器从未被调用（也不冻结屏幕 OCR 槽位），快照记
                // NotCalled —— 没有 prompt 可拼，绝不事后伪造。
                let skip_reason = if skips_for_short_text {
                    format!(
                        "skipped polish: transcript has {} non-whitespace characters, at or below the {}-character threshold",
                        crate::dictation_context::non_whitespace_char_count(&transcript.text),
                        crate::dictation_context::POLISH_SKIP_MAX_CHARS,
                    )
                } else {
                    "raw passthrough: the polisher was bypassed".to_string()
                };
                (
                    crate::ports::PolishOutput::text(transcript.text.clone()),
                    false,
                    None,
                    Some(PolishDiagnostics::not_called_from_context(
                        &context,
                        &skip_reason,
                    )),
                )
            };
            if session.cancelled.load(Ordering::Acquire) {
                remove_session(&sessions, session_id, &session);
                return Err(
                    cancelled_error("dictation was cancelled after polishing finished").into(),
                );
            }

            remove_session(&sessions, session_id, &session);
            Ok(EngineResult {
                raw_text: transcript.text,
                asr_transcript,
                polished_text: polish_output.text,
                polish_source: polish_output.source_text,
                duration_ms: transcript.duration_ms,
                polish_failed,
                asr_ms,
                polish_ms,
                has_audio_recording,
                asr_call_label,
                llm_call_label: polish_output.llm_call_label,
                polish_diagnostics,
            })
        })
    }

    fn update_context(
        &self,
        session_id: SessionId,
        context: Arc<DictationContext>,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        let session = self
            .sessions
            .lock()
            .expect("pipeline session lock poisoned")
            .get(&session_id)
            .cloned();
        Box::pin(async move {
            let session = session.ok_or_else(|| {
                BackendError::new(
                    BackendErrorCode::InvalidState,
                    "dictation pipeline session is not active",
                )
            })?;
            session.update_context(context)
        })
    }

    fn feed_audio(&self, session_id: SessionId, pcm: &[u8]) -> Result<(), BackendError> {
        self.recorder.feed_pcm(session_id, pcm)
    }

    fn cancel(&self, session_id: SessionId) -> BoxFuture<'static, Result<(), BackendError>> {
        let sessions = Arc::clone(&self.sessions);
        let polisher = Arc::clone(&self.polisher);
        Box::pin(async move {
            let Some(session) = sessions
                .lock()
                .expect("pipeline session lock poisoned")
                .get(&session_id)
                .cloned()
            else {
                return Ok(());
            };
            if session.cancelled.swap(true, Ordering::AcqRel) {
                return Ok(());
            }

            let (recording, transcription) = {
                let mut resources = session
                    .resources
                    .lock()
                    .expect("pipeline resource lock poisoned");
                (resources.recording.take(), resources.transcription.clone())
            };
            let mut first_error = None;
            if let Some(recording) = recording {
                retain_first_error(&mut first_error, recording.stop().await);
            }
            if let Some(transcription) = transcription {
                retain_first_error(
                    &mut first_error,
                    cancel_transcription_once(&session, transcription).await,
                );
            }
            if session.polishing.load(Ordering::Acquire) {
                retain_first_error(
                    &mut first_error,
                    cancel_polisher_once(&session, &polisher, session_id).await,
                );
            }
            remove_session(&sessions, session_id, &session);
            match first_error {
                Some(error) => Err(error),
                None => Ok(()),
            }
        })
    }
}

fn find_session(
    sessions: &Arc<Mutex<HashMap<SessionId, Arc<PipelineSession>>>>,
    session_id: SessionId,
) -> Result<Arc<PipelineSession>, BackendError> {
    sessions
        .lock()
        .expect("pipeline session lock poisoned")
        .get(&session_id)
        .cloned()
        .ok_or_else(|| {
            BackendError::new(
                BackendErrorCode::InvalidState,
                "dictation pipeline session is not active",
            )
        })
}

fn remove_session(
    sessions: &Arc<Mutex<HashMap<SessionId, Arc<PipelineSession>>>>,
    session_id: SessionId,
    expected: &Arc<PipelineSession>,
) {
    let mut sessions = sessions.lock().expect("pipeline session lock poisoned");
    if sessions
        .get(&session_id)
        .is_some_and(|current| Arc::ptr_eq(current, expected))
    {
        sessions.remove(&session_id);
    }
}

async fn retry_transcription(
    engine: Arc<dyn TranscriptionEngine>,
    session: Arc<PipelineSession>,
    session_id: SessionId,
    context: Arc<DictationContext>,
    progress: Arc<dyn EngineProgressSink>,
    pcm: Vec<u8>,
) -> Result<
    (
        crate::ports::TranscriptOutput,
        Option<crate::auxiliary::AsrCallLabel>,
    ),
    (BackendError, Option<crate::auxiliary::AsrCallLabel>),
> {
    let mut last_label = None;
    for attempt in 1..=2_u64 {
        if let Err(error) =
            cancellable_backoff(&session, std::time::Duration::from_millis(500 * attempt)).await
        {
            return Err((error, last_label));
        }
        let partials: Arc<dyn TextStreamSink> = Arc::new(TranscriptProgressForwarder {
            session_id,
            progress: Arc::clone(&progress),
        });
        let transcription = match engine
            .start(session_id, Arc::clone(&context), partials)
            .await
        {
            Ok(transcription) => transcription,
            Err(_) if session.cancelled.load(Ordering::Acquire) => {
                return Err((
                    cancelled_error("dictation was cancelled while retry ASR was starting"),
                    last_label,
                ));
            }
            Err(error) if error.retryable && attempt < 2 => continue,
            Err(error) => return Err((error, last_label)),
        };
        last_label = transcription.asr_call_label();
        let registered = {
            let mut resources = session
                .resources
                .lock()
                .expect("pipeline resource lock poisoned");
            // Cancellation may have removed the pipeline while the provider
            // was creating this retry. Publish the new resource and reset its
            // once-only flags under the same lock used by cancel(), so that
            // cancel either owns this retry or the late-start path cleans it.
            if session.cancelled.load(Ordering::Acquire) {
                false
            } else {
                session
                    .transcription_cancelled
                    .store(false, Ordering::Release);
                session
                    .transcription_finished
                    .store(false, Ordering::Release);
                resources.transcription = Some(Arc::clone(&transcription));
                true
            }
        };
        if !registered {
            // The global once flag belongs to the previous attempt. This
            // unregistered resource must be cancelled directly, never fed.
            let _ = transcription.cancel().await;
            return Err((
                cancelled_error("dictation was cancelled while retry ASR was starting"),
                last_label,
            ));
        }
        if session.cancelled.load(Ordering::Acquire) {
            let _ = cancel_transcription_once(&session, transcription).await;
            return Err((
                cancelled_error("dictation was cancelled before retry audio replay"),
                last_label,
            ));
        }
        transcription.consume_pcm_chunk(&pcm);
        let transcription_result = transcription.finish().await;
        for notification in transcription.take_progress_notifications() {
            if let Err(error) = publish_progress(
                &session,
                session_id,
                &progress,
                EngineProgress::Notification(notification),
            ) {
                return Err((error, last_label));
            }
        }
        match transcription_result {
            Ok(output) => {
                session
                    .transcription_finished
                    .store(true, Ordering::Release);
                return Ok((output, last_label));
            }
            Err(error) => {
                let _ = cancel_transcription_once(&session, transcription).await;
                if !error.retryable || attempt == 2 {
                    return Err((error, last_label));
                }
            }
        }
    }
    unreachable!("retry loop always returns")
}

async fn cancellable_backoff(
    session: &PipelineSession,
    duration: std::time::Duration,
) -> Result<(), BackendError> {
    let deadline = tokio::time::Instant::now() + duration;
    loop {
        if session.cancelled.load(Ordering::Acquire) {
            return Err(cancelled_error(
                "dictation was cancelled during ASR retry backoff",
            ));
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        tokio::time::sleep(remaining.min(std::time::Duration::from_millis(25))).await;
    }
}

async fn cancel_transcription_once(
    session: &Arc<PipelineSession>,
    transcription: Arc<dyn TranscriptionSession>,
) -> Result<(), BackendError> {
    if session.transcription_finished.load(Ordering::Acquire)
        || session.transcription_cancelled.swap(true, Ordering::AcqRel)
    {
        return Ok(());
    }
    transcription.cancel().await
}

async fn cancel_polisher_once(
    session: &Arc<PipelineSession>,
    polisher: &Arc<dyn TextPolisher>,
    session_id: SessionId,
) -> Result<(), BackendError> {
    if session.polisher_cancelled.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    polisher.cancel(session_id).await
}

fn publish_progress(
    session: &Arc<PipelineSession>,
    session_id: SessionId,
    progress: &Arc<dyn EngineProgressSink>,
    event: EngineProgress,
) -> Result<(), BackendError> {
    if session.cancelled.load(Ordering::Acquire) {
        return Err(cancelled_error(
            "dictation progress arrived after cancellation",
        ));
    }
    progress.publish(session_id, event)
}

fn can_fallback_to_raw(policy: PolishFailurePolicy, error: &BackendError) -> bool {
    policy == PolishFailurePolicy::UseRawText
        && matches!(
            error.code,
            BackendErrorCode::Provider
                | BackendErrorCode::Unsupported
                // 本地 Gemma 生成预算耗尽（停不下来）：没有可用的润色结果，
                // 但原文完整，回退原文比整条听写失败更符合用户预期。
                | BackendErrorCode::GenerationBudgetExceeded
        )
}

fn cancelled_error(message: &'static str) -> BackendError {
    BackendError::new(BackendErrorCode::Cancelled, message)
}

fn retain_first_error(first_error: &mut Option<BackendError>, result: Result<(), BackendError>) {
    if first_error.is_none() {
        if let Err(error) = result {
            *first_error = Some(error);
        }
    }
}

struct SessionAudioConsumer {
    session: Arc<dyn TranscriptionSession>,
    pcm_signal: Arc<AudioSignalProbe>,
}

impl AudioConsumer for SessionAudioConsumer {
    fn consume_pcm_chunk(&self, pcm: &[u8]) {
        self.pcm_signal.observe(pcm);
        self.session.consume_pcm_chunk(pcm);
    }
}

struct RecordingProgressForwarder {
    session_id: SessionId,
    session: Arc<PipelineSession>,
    progress: Arc<dyn EngineProgressSink>,
}

impl RecordingProgressSink for RecordingProgressForwarder {
    fn publish_level(&self, elapsed_ms: u64, level: f32) -> Result<(), BackendError> {
        self.progress.publish(
            self.session_id,
            EngineProgress::RecordingLevel {
                elapsed_ms,
                level: level.clamp(0.0, 1.0),
            },
        )
    }

    fn publish(&self, event: crate::ports::RecordingEvent) -> Result<(), BackendError> {
        match event {
            crate::ports::RecordingEvent::Level { elapsed_ms, level } => {
                self.publish_level(elapsed_ms, level)
            }
            crate::ports::RecordingEvent::Fatal(error) => {
                *self
                    .session
                    .recording_fault
                    .lock()
                    .expect("recording fault lock poisoned") = Some(error.clone());
                self.progress
                    .publish(self.session_id, EngineProgress::RecordingFault(error))
            }
        }
    }
}

struct TranscriptProgressForwarder {
    session_id: SessionId,
    progress: Arc<dyn EngineProgressSink>,
}

impl TextStreamSink for TranscriptProgressForwarder {
    fn publish(&self, chunk: TextStreamChunk) -> Result<(), BackendError> {
        self.progress.publish(
            self.session_id,
            EngineProgress::TranscriptDelta(TranscriptDelta {
                text: chunk.text,
                offset: chunk.offset,
                is_final: false,
            }),
        )
    }
}

struct NoopTextStreamSink;

impl TextStreamSink for NoopTextStreamSink {
    fn publish(&self, _chunk: TextStreamChunk) -> Result<(), BackendError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;
    use crate::ports::{RecordingArchive, TranscriptOutput};

    #[derive(Default)]
    struct RecordingProgress {
        events: Mutex<Vec<EngineProgress>>,
    }

    impl EngineProgressSink for RecordingProgress {
        fn publish(
            &self,
            _session_id: SessionId,
            progress: EngineProgress,
        ) -> Result<(), BackendError> {
            self.events.lock().unwrap().push(progress);
            Ok(())
        }
    }

    struct FixtureRecording {
        stops: Arc<AtomicUsize>,
        archive: Arc<FixtureArchive>,
    }

    struct FixtureArchive {
        available: AtomicBool,
        discards: Arc<AtomicUsize>,
        pcm: Vec<u8>,
    }

    impl RecordingArchive for FixtureArchive {
        fn is_available(&self) -> bool {
            self.available.load(Ordering::Acquire)
        }

        fn discard(&self) -> BoxFuture<'static, Result<(), BackendError>> {
            self.discards.fetch_add(1, Ordering::AcqRel);
            self.available.store(false, Ordering::Release);
            Box::pin(async { Ok(()) })
        }

        fn read_pcm(&self) -> BoxFuture<'static, Result<Vec<u8>, BackendError>> {
            let pcm = self.pcm.clone();
            Box::pin(async move { Ok(pcm) })
        }
    }

    impl ActiveRecording for FixtureRecording {
        fn archive(&self) -> Option<Arc<dyn RecordingArchive>> {
            Some(self.archive.clone())
        }

        fn stop(self: Box<Self>) -> BoxFuture<'static, Result<(), BackendError>> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }

    struct FixtureRecorder {
        stops: Arc<AtomicUsize>,
        archive: Arc<FixtureArchive>,
        fail: bool,
    }

    impl AudioRecorder for FixtureRecorder {
        fn start(
            &self,
            _session_id: SessionId,
            _context: Arc<DictationContext>,
            consumer: Arc<dyn AudioConsumer>,
            progress: Arc<dyn RecordingProgressSink>,
        ) -> BoxFuture<'static, Result<Box<dyn ActiveRecording>, BackendError>> {
            let stops = Arc::clone(&self.stops);
            let archive = Arc::clone(&self.archive);
            let fail = self.fail;
            Box::pin(async move {
                if fail {
                    return Err(BackendError::new(
                        BackendErrorCode::Platform,
                        "fixture recorder failed",
                    ));
                }
                consumer.consume_pcm_chunk(&[1, 0, 2, 0]);
                progress.publish_level(25, 1.5)?;
                Ok(Box::new(FixtureRecording { stops, archive }) as Box<dyn ActiveRecording>)
            })
        }
    }

    /// 按固定 PCM 喂下游的录音器：覆盖全零、零长度和安静但非零的录音。
    struct FixedPcmRecorder {
        pcm: Vec<u8>,
        stops: Arc<AtomicUsize>,
        archive: Arc<FixtureArchive>,
    }

    impl AudioRecorder for FixedPcmRecorder {
        fn start(
            &self,
            _session_id: SessionId,
            _context: Arc<DictationContext>,
            consumer: Arc<dyn AudioConsumer>,
            _progress: Arc<dyn RecordingProgressSink>,
        ) -> BoxFuture<'static, Result<Box<dyn ActiveRecording>, BackendError>> {
            let pcm = self.pcm.clone();
            let stops = Arc::clone(&self.stops);
            let archive = Arc::clone(&self.archive);
            Box::pin(async move {
                if !pcm.is_empty() {
                    consumer.consume_pcm_chunk(&pcm);
                }
                Ok(Box::new(FixtureRecording { stops, archive }) as Box<dyn ActiveRecording>)
            })
        }
    }

    struct FixtureTranscriptionSession {
        /// 本次会话返回的 ASR 文本（短输入跳润色的测试需要可变的字数）。
        text: String,
        pcm: Arc<Mutex<Vec<u8>>>,
        cancels: Arc<AtomicUsize>,
        finishes: Arc<AtomicUsize>,
        finish_entered: Option<Arc<tokio::sync::Notify>>,
        finish_release: Option<Arc<tokio::sync::Notify>>,
    }

    impl AudioConsumer for FixtureTranscriptionSession {
        fn consume_pcm_chunk(&self, pcm: &[u8]) {
            self.pcm.lock().unwrap().extend_from_slice(pcm);
        }
    }

    impl TranscriptionSession for FixtureTranscriptionSession {
        fn finish(&self) -> BoxFuture<'static, Result<TranscriptOutput, BackendError>> {
            self.finishes.fetch_add(1, Ordering::SeqCst);
            let entered = self.finish_entered.clone();
            let release = self.finish_release.clone();
            let text = self.text.clone();
            Box::pin(async move {
                if let Some(entered) = entered {
                    entered.notify_one();
                }
                if let Some(release) = release {
                    release.notified().await;
                }
                Ok(TranscriptOutput {
                    text,
                    duration_ms: 25,
                })
            })
        }

        fn cancel(&self) -> BoxFuture<'static, Result<(), BackendError>> {
            self.cancels.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }

    struct FixtureTranscriber {
        session: Arc<FixtureTranscriptionSession>,
    }

    impl TranscriptionEngine for FixtureTranscriber {
        fn start(
            &self,
            _session_id: SessionId,
            _context: Arc<DictationContext>,
            partials: Arc<dyn TextStreamSink>,
        ) -> BoxFuture<'static, Result<Arc<dyn TranscriptionSession>, BackendError>> {
            let session = Arc::clone(&self.session);
            Box::pin(async move {
                partials.publish(TextStreamChunk {
                    text: "raw".to_string(),
                    offset: 0,
                })?;
                Ok(session as Arc<dyn TranscriptionSession>)
            })
        }
    }

    struct RetryTranscriber {
        outputs: Arc<Mutex<VecDeque<Result<TranscriptOutput, BackendError>>>>,
        starts: Arc<AtomicUsize>,
        pcm: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    struct RetryTranscriptionSession {
        output: Result<TranscriptOutput, BackendError>,
        pcm: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl AudioConsumer for RetryTranscriptionSession {
        fn consume_pcm_chunk(&self, pcm: &[u8]) {
            self.pcm.lock().unwrap().push(pcm.to_vec());
        }
    }

    impl TranscriptionSession for RetryTranscriptionSession {
        fn finish(&self) -> BoxFuture<'static, Result<TranscriptOutput, BackendError>> {
            let output = self.output.clone();
            Box::pin(async move { output })
        }

        fn cancel(&self) -> BoxFuture<'static, Result<(), BackendError>> {
            Box::pin(async { Ok(()) })
        }
    }

    impl TranscriptionEngine for RetryTranscriber {
        fn start(
            &self,
            _session_id: SessionId,
            _context: Arc<DictationContext>,
            _partials: Arc<dyn TextStreamSink>,
        ) -> BoxFuture<'static, Result<Arc<dyn TranscriptionSession>, BackendError>> {
            self.starts.fetch_add(1, Ordering::AcqRel);
            let output = self
                .outputs
                .lock()
                .unwrap()
                .pop_front()
                .expect("retry fixture output");
            let pcm = Arc::clone(&self.pcm);
            Box::pin(async move {
                Ok(Arc::new(RetryTranscriptionSession { output, pcm })
                    as Arc<dyn TranscriptionSession>)
            })
        }
    }

    struct FixturePolisher {
        result: Result<crate::ports::PolishOutput, BackendError>,
        calls: Arc<AtomicUsize>,
        cancels: Arc<AtomicUsize>,
        contexts: Arc<Mutex<Vec<Arc<DictationContext>>>>,
    }

    impl TextPolisher for FixturePolisher {
        fn polish(
            &self,
            _session_id: SessionId,
            context: Arc<DictationContext>,
            _raw_text: String,
            partials: Arc<dyn TextStreamSink>,
        ) -> BoxFuture<'static, crate::ports::PolishAttempt> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.contexts.lock().unwrap().push(Arc::clone(&context));
            let result = self.result.clone();
            // 测试桩的诊断：成功/失败都带一个最小快照，便于断言持久化链路；
            // 失败路径镜像 local_llm 的行为（错误摘要进快照、阶段标 Tokenized）。
            let mut diagnostics =
                crate::polish_diagnostics::PolishDiagnostics::not_called_from_context(
                    &context,
                    "fixture polisher marker",
                );
            if let Err(error) = &self.result {
                diagnostics.stage = crate::polish_diagnostics::PolishDiagnosticsStage::Tokenized;
                diagnostics.outcome = crate::polish_diagnostics::PolishDiagnosticsOutcome::Failed;
                diagnostics.not_called_reason = None;
                diagnostics.prompt = Some("<|turn>system\nfixture<|turn>model\n".to_string());
                diagnostics.error = Some(crate::polish_diagnostics::PolishDiagnosticError {
                    code: format!("{:?}", error.code),
                    message: error.message.clone(),
                });
            }
            Box::pin(async move {
                let result = match result {
                    Ok(output) => match partials.publish(TextStreamChunk {
                        text: "polished".to_string(),
                        offset: 0,
                    }) {
                        Ok(()) => Ok(output),
                        Err(error) => Err(error),
                    },
                    Err(error) => Err(error),
                };
                crate::ports::PolishAttempt {
                    result,
                    diagnostics: Some(diagnostics),
                }
            })
        }

        fn cancel(&self, _session_id: SessionId) -> BoxFuture<'static, Result<(), BackendError>> {
            self.cancels.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }

    struct FixtureParts {
        engine: PipelineDictationEngine,
        progress: Arc<RecordingProgress>,
        pcm: Arc<Mutex<Vec<u8>>>,
        recorder_stops: Arc<AtomicUsize>,
        archive_discards: Arc<AtomicUsize>,
        transcription_cancels: Arc<AtomicUsize>,
        polish_calls: Arc<AtomicUsize>,
        polish_contexts: Arc<Mutex<Vec<Arc<DictationContext>>>>,
    }

    fn fixture_engine(
        recorder_fails: bool,
        polish_result: Result<crate::ports::PolishOutput, BackendError>,
        finish_entered: Option<Arc<tokio::sync::Notify>>,
        finish_release: Option<Arc<tokio::sync::Notify>>,
    ) -> FixtureParts {
        fixture_engine_with_transcript(
            "raw text",
            recorder_fails,
            polish_result,
            finish_entered,
            finish_release,
        )
    }

    fn fixture_engine_with_transcript(
        transcript: &str,
        recorder_fails: bool,
        polish_result: Result<crate::ports::PolishOutput, BackendError>,
        finish_entered: Option<Arc<tokio::sync::Notify>>,
        finish_release: Option<Arc<tokio::sync::Notify>>,
    ) -> FixtureParts {
        let pcm = Arc::new(Mutex::new(Vec::new()));
        let recorder_stops = Arc::new(AtomicUsize::new(0));
        let archive_discards = Arc::new(AtomicUsize::new(0));
        let archive = Arc::new(FixtureArchive {
            available: AtomicBool::new(true),
            discards: Arc::clone(&archive_discards),
            pcm: vec![1, 0, 2, 0],
        });
        let transcription_cancels = Arc::new(AtomicUsize::new(0));
        let polish_calls = Arc::new(AtomicUsize::new(0));
        let polish_contexts = Arc::new(Mutex::new(Vec::new()));
        let transcriber = Arc::new(FixtureTranscriber {
            session: Arc::new(FixtureTranscriptionSession {
                text: transcript.to_string(),
                pcm: Arc::clone(&pcm),
                cancels: Arc::clone(&transcription_cancels),
                finishes: Arc::new(AtomicUsize::new(0)),
                finish_entered,
                finish_release,
            }),
        });
        let engine = PipelineDictationEngine::new(
            Arc::new(FixtureRecorder {
                stops: Arc::clone(&recorder_stops),
                archive,
                fail: recorder_fails,
            }),
            transcriber,
            Arc::new(FixturePolisher {
                result: polish_result,
                calls: Arc::clone(&polish_calls),
                cancels: Arc::new(AtomicUsize::new(0)),
                contexts: Arc::clone(&polish_contexts),
            }),
        );
        FixtureParts {
            engine,
            progress: Arc::new(RecordingProgress::default()),
            pcm,
            recorder_stops,
            archive_discards,
            transcription_cancels,
            polish_calls,
            polish_contexts,
        }
    }

    #[tokio::test]
    async fn pipeline_streams_pcm_progress_and_terminal_deltas() {
        let fixture = fixture_engine(
            false,
            Ok(crate::ports::PolishOutput::text("polished text")),
            None,
            None,
        );
        let session_id = SessionId::new();
        fixture
            .engine
            .start(
                session_id,
                Arc::new(DictationContext::default()),
                fixture.progress.clone(),
            )
            .await
            .unwrap();
        let result = fixture
            .engine
            .finish(session_id, fixture.progress.clone())
            .await
            .unwrap();

        assert_eq!(&*fixture.pcm.lock().unwrap(), &[1, 0, 2, 0]);
        assert_eq!(fixture.recorder_stops.load(Ordering::SeqCst), 1);
        assert_eq!(result.raw_text, "raw text");
        assert_eq!(result.polished_text, "polished text");
        // 成功也带诊断快照（来自 polisher 的 attempt，不是成功独占字段）。
        let diagnostics = result
            .polish_diagnostics
            .as_ref()
            .expect("success carries the polish attempt diagnostics");
        assert_eq!(
            diagnostics.not_called_reason.as_deref(),
            Some("fixture polisher marker")
        );
        let events = fixture.progress.events.lock().unwrap();
        assert!(events.contains(&EngineProgress::RecordingLevel {
            elapsed_ms: 25,
            level: 1.0,
        }));
        assert!(
            events.contains(&EngineProgress::TranscriptDelta(TranscriptDelta {
                text: "raw text".to_string(),
                offset: 0,
                is_final: true,
            }))
        );
    }

    #[tokio::test]
    async fn short_transcript_skips_polish_and_records_not_called() {
        // "确定重试" 是 4 个非空白字符，正好在 POLISH_SKIP_MAX_CHARS 门槛上。
        let fixture = fixture_engine_with_transcript(
            "确定重试",
            false,
            Ok(crate::ports::PolishOutput::text("polished text")),
            None,
            None,
        );
        let session_id = SessionId::new();
        fixture
            .engine
            .start(
                session_id,
                Arc::new(DictationContext::default()),
                fixture.progress.clone(),
            )
            .await
            .unwrap();
        let result = fixture
            .engine
            .finish(session_id, fixture.progress.clone())
            .await
            .unwrap();

        assert_eq!(fixture.polish_calls.load(Ordering::SeqCst), 0);
        // raw 插入路径：上屏的就是 ASR 结果（词表纠正发生在上游适配器）。
        assert_eq!(result.raw_text, "确定重试");
        assert_eq!(result.polished_text, "确定重试");
        assert!(!result.polish_failed);
        assert_eq!(result.polish_ms, None);
        // 没有进入润色阶段，前端不会显示润色进度。
        assert!(!fixture
            .progress
            .events
            .lock()
            .unwrap()
            .contains(&EngineProgress::Stage(EngineStage::Polishing)));

        // 诊断如实记「未调用」与原因：没有 prompt，绝不事后伪造。
        let diagnostics = result
            .polish_diagnostics
            .as_ref()
            .expect("short-input skip carries diagnostics");
        assert_eq!(
            diagnostics.stage,
            crate::polish_diagnostics::PolishDiagnosticsStage::NotCalled
        );
        assert_eq!(
            diagnostics.outcome,
            crate::polish_diagnostics::PolishDiagnosticsOutcome::NotCalled
        );
        assert!(diagnostics.prompt.is_none());
        assert!(diagnostics.messages.is_none());
        let reason = diagnostics
            .not_called_reason
            .as_deref()
            .expect("skip reason");
        assert!(reason.contains("skipped polish"), "reason: {reason}");
        assert!(reason.contains('4'), "reason: {reason}");
    }

    #[tokio::test]
    async fn transcript_over_the_short_limit_still_polishes() {
        // "确定重试好" 是 5 个非空白字符，超过门槛，照常调用润色器。
        let fixture = fixture_engine_with_transcript(
            "确定重试好",
            false,
            Ok(crate::ports::PolishOutput::text("polished text")),
            None,
            None,
        );
        let session_id = SessionId::new();
        fixture
            .engine
            .start(
                session_id,
                Arc::new(DictationContext::default()),
                fixture.progress.clone(),
            )
            .await
            .unwrap();
        let result = fixture
            .engine
            .finish(session_id, fixture.progress.clone())
            .await
            .unwrap();

        assert_eq!(fixture.polish_calls.load(Ordering::SeqCst), 1);
        assert_eq!(result.polished_text, "polished text");
        assert!(!result.polish_failed);
    }

    fn pcm_validation_engine(
        pcm: Vec<u8>,
    ) -> (
        PipelineDictationEngine,
        Arc<RecordingProgress>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        Arc<Mutex<Vec<u8>>>,
    ) {
        let received = Arc::new(Mutex::new(Vec::new()));
        let finishes = Arc::new(AtomicUsize::new(0));
        let cancels = Arc::new(AtomicUsize::new(0));
        let engine = PipelineDictationEngine::new(
            Arc::new(FixedPcmRecorder {
                pcm,
                stops: Arc::new(AtomicUsize::new(0)),
                archive: Arc::new(FixtureArchive {
                    available: AtomicBool::new(true),
                    discards: Arc::new(AtomicUsize::new(0)),
                    pcm: Vec::new(),
                }),
            }),
            Arc::new(FixtureTranscriber {
                session: Arc::new(FixtureTranscriptionSession {
                    text: "raw text".to_string(),
                    pcm: Arc::clone(&received),
                    cancels: Arc::clone(&cancels),
                    finishes: Arc::clone(&finishes),
                    finish_entered: None,
                    finish_release: None,
                }),
            }),
            Arc::new(FixturePolisher {
                result: Ok(crate::ports::PolishOutput::text("must not run")),
                calls: Arc::new(AtomicUsize::new(0)),
                cancels: Arc::new(AtomicUsize::new(0)),
                contexts: Arc::new(Mutex::new(Vec::new())),
            }),
        );
        (
            engine,
            Arc::new(RecordingProgress::default()),
            finishes,
            cancels,
            received,
        )
    }

    #[tokio::test]
    async fn empty_or_all_zero_pcm_fails_explicitly_without_finalizing_asr() {
        for pcm in [Vec::new(), vec![0u8; 640]] {
            let (engine, progress, finishes, cancels, received) =
                pcm_validation_engine(pcm.clone());
            let session_id = SessionId::new();
            engine
                .start(session_id, raw_dictation_context(), progress.clone())
                .await
                .unwrap();

            let failure = engine.finish(session_id, progress).await.unwrap_err();

            assert_eq!(failure.error.code, BackendErrorCode::NoAudio);
            assert!(!failure.error.retryable);
            assert_eq!(
                failure.error.message,
                "未收到有效音频，请检查麦克风输入设备后重试"
            );
            assert_eq!(failure.stage, EngineFailureStage::Transcribing);
            assert_eq!(
                finishes.load(Ordering::SeqCst),
                0,
                "全零/零长度 PCM 不得进入 ASR finalize"
            );
            assert_eq!(cancels.load(Ordering::SeqCst), 1, "ASR 会话被取消释放");
            assert_eq!(
                &*received.lock().unwrap(),
                &pcm,
                "PCM 照常转发给 ASR 会话，只是不 finalize"
            );
        }
    }

    #[tokio::test]
    async fn quiet_nonzero_pcm_is_not_rejected_as_missing_audio() {
        let pcm = vec![1u8, 0];
        let (engine, progress, finishes, cancels, received) = pcm_validation_engine(pcm.clone());
        let session_id = SessionId::new();
        engine
            .start(session_id, raw_dictation_context(), progress.clone())
            .await
            .unwrap();

        let result = engine.finish(session_id, progress).await.unwrap();

        assert_eq!(result.raw_text, "raw text");
        assert_eq!(finishes.load(Ordering::SeqCst), 1);
        assert_eq!(cancels.load(Ordering::SeqCst), 0);
        assert_eq!(&*received.lock().unwrap(), &pcm);
    }

    #[tokio::test]
    async fn pipeline_uses_the_updated_context_when_finalizing() {
        let fixture = fixture_engine(
            false,
            Ok(crate::ports::PolishOutput::text("translated text")),
            None,
            None,
        );
        let session_id = SessionId::new();
        let initial = Arc::new(DictationContext::default());
        fixture
            .engine
            .start(session_id, initial.clone(), fixture.progress.clone())
            .await
            .unwrap();
        let updated = Arc::new((*initial).clone());

        fixture
            .engine
            .update_context(session_id, updated.clone())
            .await
            .unwrap();
        fixture
            .engine
            .finish(session_id, fixture.progress.clone())
            .await
            .unwrap();

        let contexts = fixture.polish_contexts.lock().unwrap();
        assert_eq!(contexts.as_slice(), &[updated]);
    }

    #[tokio::test]
    async fn pipeline_rejects_context_updates_after_finalization_starts() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let fixture = fixture_engine(
            false,
            Ok(crate::ports::PolishOutput::text("polished text")),
            Some(Arc::clone(&entered)),
            Some(Arc::clone(&release)),
        );
        let session_id = SessionId::new();
        fixture
            .engine
            .start(
                session_id,
                Arc::new(DictationContext::default()),
                fixture.progress.clone(),
            )
            .await
            .unwrap();
        let finish = fixture.engine.finish(session_id, fixture.progress.clone());
        let finish_task = tokio::spawn(finish);
        entered.notified().await;

        let error = fixture
            .engine
            .update_context(session_id, Arc::new(DictationContext::default()))
            .await
            .expect_err("context must be frozen after finish starts");
        assert_eq!(error.code, BackendErrorCode::InvalidState);

        release.notify_one();
        finish_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn successful_transcription_discards_archive_when_debug_recording_is_disabled() {
        let fixture = fixture_engine(
            false,
            Ok(crate::ports::PolishOutput::text("polished text")),
            None,
            None,
        );
        let session_id = SessionId::new();
        fixture
            .engine
            .start(
                session_id,
                Arc::new(DictationContext::default()),
                fixture.progress.clone(),
            )
            .await
            .unwrap();

        let result = fixture
            .engine
            .finish(session_id, fixture.progress.clone())
            .await
            .unwrap();

        assert_eq!(fixture.archive_discards.load(Ordering::Acquire), 1);
        assert_eq!(result.has_audio_recording, Some(false));
    }

    #[tokio::test]
    async fn successful_transcription_preserves_archive_when_debug_recording_is_enabled() {
        let fixture = fixture_engine(
            false,
            Ok(crate::ports::PolishOutput::text("polished text")),
            None,
            None,
        );
        let session_id = SessionId::new();
        let mut context = DictationContext::default();
        context.recording.archive_successful_recording = true;
        fixture
            .engine
            .start(session_id, Arc::new(context), fixture.progress.clone())
            .await
            .unwrap();

        let result = fixture
            .engine
            .finish(session_id, fixture.progress.clone())
            .await
            .unwrap();

        assert_eq!(fixture.archive_discards.load(Ordering::Acquire), 0);
        assert_eq!(result.has_audio_recording, Some(true));
    }

    #[tokio::test]
    async fn recorder_start_failure_cancels_transcription_exactly_once() {
        let fixture = fixture_engine(
            false,
            Ok(crate::ports::PolishOutput::text("unused")),
            None,
            None,
        );
        let failing = fixture_engine(
            true,
            Ok(crate::ports::PolishOutput::text("unused")),
            None,
            None,
        );
        let session_id = SessionId::new();
        let error = failing
            .engine
            .start(
                session_id,
                Arc::new(DictationContext::default()),
                failing.progress.clone(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, BackendErrorCode::Platform);
        assert_eq!(failing.transcription_cancels.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.transcription_cancels.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn provider_polish_failure_uses_raw_text_fallback() {
        let fixture = fixture_engine(
            false,
            Err(BackendError::new(
                BackendErrorCode::Provider,
                "fixture polish failure",
            )),
            None,
            None,
        );
        let session_id = SessionId::new();
        fixture
            .engine
            .start(
                session_id,
                Arc::new(DictationContext::default()),
                fixture.progress.clone(),
            )
            .await
            .unwrap();
        let result = fixture
            .engine
            .finish(session_id, fixture.progress.clone())
            .await
            .unwrap();
        assert_eq!(result.raw_text, "raw text");
        assert_eq!(result.polished_text, "raw text");
        // 回退 raw 的快照是失败尝试的真实诊断（含渲染 prompt 与错误摘要），
        // 而不是丢弃，也不是成功快照。
        let fallback_diagnostics = result
            .polish_diagnostics
            .as_ref()
            .expect("fallback keeps the failed attempt diagnostics");
        assert_eq!(
            fallback_diagnostics.outcome,
            crate::polish_diagnostics::PolishDiagnosticsOutcome::Failed
        );
        assert!(fallback_diagnostics.prompt.is_some());
        assert!(fallback_diagnostics.error.is_some());
    }

    #[tokio::test]
    async fn generation_budget_exhaustion_uses_raw_text_fallback() {
        // 本地 Gemma 生成耗尽（停不下来）不再让整条听写失败：回退原文插入，
        // 与现有的 Provider 回退走同一条路径。
        let fixture = fixture_engine(
            false,
            Err(BackendError::new(
                BackendErrorCode::GenerationBudgetExceeded,
                "local LLM output exceeded its 1024-token limit",
            )),
            None,
            None,
        );
        let session_id = SessionId::new();
        fixture
            .engine
            .start(
                session_id,
                Arc::new(DictationContext::default()),
                fixture.progress.clone(),
            )
            .await
            .unwrap();
        let result = fixture
            .engine
            .finish(session_id, fixture.progress.clone())
            .await
            .expect("budget exhaustion falls back instead of failing the dictation");
        assert_eq!(result.raw_text, "raw text");
        assert_eq!(result.polished_text, "raw text");
        assert!(result.polish_failed);
        let diagnostics = result
            .polish_diagnostics
            .as_ref()
            .expect("fallback keeps the failed attempt diagnostics");
        assert_eq!(
            diagnostics.outcome,
            crate::polish_diagnostics::PolishDiagnosticsOutcome::Failed
        );
        assert!(diagnostics.prompt.is_some());
    }

    #[test]
    fn fallback_policy_covers_generation_budget_but_not_other_invalid_arguments() {
        let budget_error = BackendError::new(
            BackendErrorCode::GenerationBudgetExceeded,
            "local LLM output exceeded its limit",
        );
        assert!(can_fallback_to_raw(
            PolishFailurePolicy::UseRawText,
            &budget_error
        ));
        assert!(!can_fallback_to_raw(
            PolishFailurePolicy::Fail,
            &budget_error
        ));

        // 其它 InvalidArgument（prompt 超上下文、prompt 无 token）仍然照旧失败，
        // 回退面不扩大。
        let prompt_too_long =
            BackendError::new(BackendErrorCode::InvalidArgument, "prompt is too long");
        assert!(!can_fallback_to_raw(
            PolishFailurePolicy::UseRawText,
            &prompt_too_long
        ));

        let provider_error = BackendError::new(BackendErrorCode::Provider, "provider failed");
        assert!(can_fallback_to_raw(
            PolishFailurePolicy::UseRawText,
            &provider_error
        ));
        let cancelled = BackendError::new(BackendErrorCode::Cancelled, "cancelled");
        assert!(!can_fallback_to_raw(
            PolishFailurePolicy::UseRawText,
            &cancelled
        ));
    }

    #[tokio::test]
    async fn duplicate_start_keeps_the_original_pipeline_session() {
        let fixture = fixture_engine(
            false,
            Ok(crate::ports::PolishOutput::text("polished text")),
            None,
            None,
        );
        let session_id = SessionId::new();
        fixture
            .engine
            .start(
                session_id,
                Arc::new(DictationContext::default()),
                fixture.progress.clone(),
            )
            .await
            .unwrap();

        let error = fixture
            .engine
            .start(
                session_id,
                Arc::new(DictationContext::default()),
                fixture.progress.clone(),
            )
            .await
            .expect_err("duplicate session must be rejected");
        assert_eq!(error.code, BackendErrorCode::Busy);

        let result = fixture
            .engine
            .finish(session_id, fixture.progress.clone())
            .await
            .expect("the original pipeline session must remain active");
        assert_eq!(result.polished_text, "polished text");
    }

    /// 不可回退的润色失败：EngineFailure 也必须携带 polisher 的诊断快照
    /// （含渲染出的 prompt 与失败阶段），否则失败记录无从诊断。
    #[tokio::test]
    async fn non_fallback_polish_failure_carries_diagnostics_on_the_failure() {
        let fixture = fixture_engine(
            false,
            Err(BackendError::new(
                BackendErrorCode::InvalidArgument,
                "local LLM output exceeded its 1-token limit after a 9-token prompt",
            )),
            None,
            None,
        );
        let session_id = SessionId::new();
        fixture
            .engine
            .start(
                session_id,
                Arc::new(DictationContext::default()),
                fixture.progress.clone(),
            )
            .await
            .unwrap();
        let failure = fixture
            .engine
            .finish(session_id, fixture.progress.clone())
            .await
            .unwrap_err();
        assert_eq!(failure.stage, EngineFailureStage::Polishing);
        let diagnostics = failure
            .polish_diagnostics
            .as_ref()
            .expect("hard polish failure carries diagnostics");
        assert_eq!(
            diagnostics.outcome,
            crate::polish_diagnostics::PolishDiagnosticsOutcome::Failed
        );
        assert!(diagnostics.prompt.is_some(), "失败快照保留渲染出的 prompt");
        // 错误信息进快照的是脱敏摘要（计数），不是正文。
        assert!(diagnostics
            .error
            .as_ref()
            .is_some_and(|error| error.message.contains("1-token limit")));
    }

    #[tokio::test]
    async fn builtin_raw_mode_bypasses_the_polisher() {
        let fixture = fixture_engine(
            false,
            Ok(crate::ports::PolishOutput::text("must not be used")),
            None,
            None,
        );
        let session_id = SessionId::new();
        let context = DictationContext::default();
        fixture
            .engine
            .start(session_id, Arc::new(context), fixture.progress.clone())
            .await
            .unwrap();
        let result = fixture
            .engine
            .finish(session_id, fixture.progress.clone())
            .await
            .unwrap();

        assert_eq!(result.polished_text, "must not be used");
        assert_eq!(fixture.polish_calls.load(Ordering::SeqCst), 1);
        let diagnostics = result.polish_diagnostics.expect("fixture diagnostics");
        assert_eq!(
            diagnostics.not_called_reason.as_deref(),
            Some("fixture polisher marker"),
            "keep the actual fixture snapshot, not a Raw bypass"
        );
    }

    #[tokio::test]
    async fn cancel_while_asr_finishes_rejects_late_terminal_progress() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let fixture = fixture_engine(
            false,
            Ok(crate::ports::PolishOutput::text("polished text")),
            Some(Arc::clone(&entered)),
            Some(Arc::clone(&release)),
        );
        let session_id = SessionId::new();
        fixture
            .engine
            .start(
                session_id,
                Arc::new(DictationContext::default()),
                fixture.progress.clone(),
            )
            .await
            .unwrap();
        let finish = fixture.engine.finish(session_id, fixture.progress.clone());
        let finish_task = tokio::spawn(finish);
        entered.notified().await;
        fixture.engine.cancel(session_id).await.unwrap();
        release.notify_one();

        let error = finish_task.await.unwrap().unwrap_err();
        assert_eq!(error.error.code, BackendErrorCode::Cancelled);
        assert_eq!(fixture.transcription_cancels.load(Ordering::SeqCst), 1);
        let events = fixture.progress.events.lock().unwrap();
        assert!(!events.iter().any(|event| matches!(
            event,
            EngineProgress::TranscriptDelta(TranscriptDelta { is_final: true, .. })
        )));
    }

    #[tokio::test]
    async fn retryable_asr_failure_reuses_the_frozen_archive_once() {
        let archive = Arc::new(FixtureArchive {
            available: AtomicBool::new(true),
            discards: Arc::new(AtomicUsize::new(0)),
            pcm: vec![1, 0, 2, 0],
        });
        let starts = Arc::new(AtomicUsize::new(0));
        let pcm = Arc::new(Mutex::new(Vec::new()));
        let transcriber = Arc::new(RetryTranscriber {
            outputs: Arc::new(Mutex::new(VecDeque::from([
                Err(BackendError::new(BackendErrorCode::Provider, "temporary").retryable(true)),
                Ok(TranscriptOutput {
                    text: "retry success".into(),
                    duration_ms: 25,
                }),
            ]))),
            starts: Arc::clone(&starts),
            pcm: Arc::clone(&pcm),
        });
        let engine = PipelineDictationEngine::new(
            Arc::new(FixtureRecorder {
                stops: Arc::new(AtomicUsize::new(0)),
                archive,
                fail: false,
            }),
            transcriber,
            Arc::new(FixturePolisher {
                result: Ok(crate::ports::PolishOutput::text("unused")),
                calls: Arc::new(AtomicUsize::new(0)),
                cancels: Arc::new(AtomicUsize::new(0)),
                contexts: Arc::new(Mutex::new(Vec::new())),
            }),
        );
        let progress: Arc<RecordingProgress> = Arc::new(RecordingProgress::default());
        let session_id = SessionId::new();
        let context = DictationContext::default();
        engine
            .start(session_id, Arc::new(context), progress.clone())
            .await
            .unwrap();

        let result = engine.finish(session_id, progress).await.unwrap();

        assert_eq!(result.raw_text, "retry success");
        assert_eq!(starts.load(Ordering::Acquire), 2);
        assert_eq!(
            pcm.lock().unwrap().as_slice(),
            &[vec![1, 0, 2, 0], vec![1, 0, 2, 0]]
        );
    }

    fn retry_test_engine(
        outputs: Vec<Result<TranscriptOutput, BackendError>>,
        archive_available: bool,
    ) -> (
        PipelineDictationEngine,
        Arc<RecordingProgress>,
        Arc<AtomicUsize>,
    ) {
        let archive = Arc::new(FixtureArchive {
            available: AtomicBool::new(archive_available),
            discards: Arc::new(AtomicUsize::new(0)),
            pcm: vec![1, 0, 2, 0],
        });
        let starts = Arc::new(AtomicUsize::new(0));
        let engine = PipelineDictationEngine::new(
            Arc::new(FixtureRecorder {
                stops: Arc::new(AtomicUsize::new(0)),
                archive,
                fail: false,
            }),
            Arc::new(RetryTranscriber {
                outputs: Arc::new(Mutex::new(outputs.into())),
                starts: Arc::clone(&starts),
                pcm: Arc::new(Mutex::new(Vec::new())),
            }),
            Arc::new(FixturePolisher {
                result: Ok(crate::ports::PolishOutput::text("unused")),
                calls: Arc::new(AtomicUsize::new(0)),
                cancels: Arc::new(AtomicUsize::new(0)),
                contexts: Arc::new(Mutex::new(Vec::new())),
            }),
        );
        (engine, Arc::new(RecordingProgress::default()), starts)
    }

    fn raw_dictation_context() -> Arc<DictationContext> {
        Arc::new(DictationContext::default())
    }

    #[tokio::test]
    async fn retryable_asr_failure_stops_after_two_retries() {
        let temporary =
            || Err(BackendError::new(BackendErrorCode::Provider, "temporary").retryable(true));
        let (engine, progress, starts) =
            retry_test_engine(vec![temporary(), temporary(), temporary()], true);
        let session_id = SessionId::new();
        engine
            .start(session_id, raw_dictation_context(), progress.clone())
            .await
            .unwrap();

        let failure = engine.finish(session_id, progress).await.unwrap_err();

        assert_eq!(failure.error.code, BackendErrorCode::Provider);
        assert_eq!(starts.load(Ordering::Acquire), 3);
    }

    #[tokio::test]
    async fn terminal_empty_and_missing_archive_never_retry() {
        let terminal = Err(BackendError::new(BackendErrorCode::Provider, "terminal"));
        let (engine, progress, starts) = retry_test_engine(vec![terminal], true);
        let session_id = SessionId::new();
        engine
            .start(session_id, raw_dictation_context(), progress.clone())
            .await
            .unwrap();
        assert_eq!(
            engine
                .finish(session_id, progress)
                .await
                .unwrap_err()
                .error
                .code,
            BackendErrorCode::Provider
        );
        assert_eq!(starts.load(Ordering::Acquire), 1);

        let (engine, progress, starts) = retry_test_engine(
            vec![Ok(TranscriptOutput {
                text: String::new(),
                duration_ms: 25,
            })],
            true,
        );
        let session_id = SessionId::new();
        engine
            .start(session_id, raw_dictation_context(), progress.clone())
            .await
            .unwrap();
        assert!(engine
            .finish(session_id, progress)
            .await
            .unwrap()
            .raw_text
            .is_empty());
        assert_eq!(starts.load(Ordering::Acquire), 1);

        let retryable =
            Err(BackendError::new(BackendErrorCode::Provider, "temporary").retryable(true));
        let (engine, progress, starts) = retry_test_engine(vec![retryable], false);
        let session_id = SessionId::new();
        engine
            .start(session_id, raw_dictation_context(), progress.clone())
            .await
            .unwrap();
        assert_eq!(
            engine
                .finish(session_id, progress)
                .await
                .unwrap_err()
                .error
                .code,
            BackendErrorCode::Provider
        );
        assert_eq!(starts.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn cancellation_during_asr_retry_backoff_starts_no_retry_session() {
        let retryable =
            Err(BackendError::new(BackendErrorCode::Provider, "temporary").retryable(true));
        let (engine, progress, starts) = retry_test_engine(vec![retryable], true);
        let engine = Arc::new(engine);
        let session_id = SessionId::new();
        engine
            .start(session_id, raw_dictation_context(), progress.clone())
            .await
            .unwrap();
        let finishing = {
            let engine = Arc::clone(&engine);
            tokio::spawn(async move { engine.finish(session_id, progress).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        engine.cancel(session_id).await.unwrap();

        let failure = finishing.await.unwrap().unwrap_err();
        assert_eq!(failure.error.code, BackendErrorCode::Cancelled);
        assert_eq!(starts.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn cancellation_while_retry_asr_starts_cancels_the_late_resource_without_feeding_it() {
        struct DelayedRetryStart {
            entered: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
            session: Arc<FixtureTranscriptionSession>,
        }
        impl TranscriptionEngine for DelayedRetryStart {
            fn start(
                &self,
                _: SessionId,
                _: Arc<DictationContext>,
                _: Arc<dyn TextStreamSink>,
            ) -> BoxFuture<'static, Result<Arc<dyn TranscriptionSession>, BackendError>>
            {
                let entered = self.entered.clone();
                let release = self.release.clone();
                let session = self.session.clone();
                Box::pin(async move {
                    entered.notify_one();
                    release.notified().await;
                    Ok(session as Arc<dyn TranscriptionSession>)
                })
            }
        }
        let (mut engine, progress, _) = retry_test_engine(
            vec![Err(BackendError::new(
                BackendErrorCode::Provider,
                "temporary",
            )
            .retryable(true))],
            true,
        );
        let session_id = SessionId::new();
        engine
            .start(session_id, raw_dictation_context(), progress.clone())
            .await
            .unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let late = Arc::new(FixtureTranscriptionSession {
            text: "raw text".to_string(),
            pcm: Arc::new(Mutex::new(Vec::new())),
            cancels: Arc::new(AtomicUsize::new(0)),
            finishes: Arc::new(AtomicUsize::new(0)),
            finish_entered: None,
            finish_release: None,
        });
        // The first session is already registered; only the retry start blocks.
        engine.transcription = Arc::new(DelayedRetryStart {
            entered: entered.clone(),
            release: release.clone(),
            session: late.clone(),
        });
        let engine = Arc::new(engine);
        let finishing = tokio::spawn({
            let engine = engine.clone();
            async move { engine.finish(session_id, progress).await }
        });
        entered.notified().await;
        engine.cancel(session_id).await.unwrap();
        release.notify_one();
        let failure = finishing.await.unwrap().unwrap_err();
        assert_eq!(failure.error.code, BackendErrorCode::Cancelled);
        assert_eq!(
            late.cancels.load(Ordering::Acquire),
            1,
            "late ASR resource needs its own cleanup"
        );
        assert!(
            late.pcm.lock().unwrap().is_empty(),
            "cancelled speech must not be replayed to a new ASR"
        );
        assert!(engine.sessions.lock().unwrap().is_empty());
    }

    // ── 屏幕 OCR：润色输入冻结点的非阻塞收取 ──────────────────────────────

    /// 可控的假 OCR 槽位：`ready` 决定 try_take 是否有结果，计数器记录探测/作废。
    #[derive(Default)]
    struct FakeScreenOcrSlot {
        ready: Mutex<Option<String>>,
        takes: AtomicUsize,
        discards: AtomicUsize,
    }

    impl FakeScreenOcrSlot {
        fn with_ready(text: &str) -> Arc<Self> {
            Arc::new(Self {
                ready: Mutex::new(Some(text.to_string())),
                ..Self::default()
            })
        }

        fn pending() -> Arc<Self> {
            Arc::new(Self::default())
        }
    }

    impl crate::ports::ScreenOcrSlot for FakeScreenOcrSlot {
        fn try_take(&self) -> Option<String> {
            self.takes.fetch_add(1, Ordering::SeqCst);
            self.ready.lock().unwrap().take()
        }

        fn discard(&self) {
            self.discards.fetch_add(1, Ordering::SeqCst);
            *self.ready.lock().unwrap() = None;
        }
    }

    /// 记录 start() 是否被调用过的录音器包装（判据=进入 recorder.start）。
    struct FlaggingRecorder {
        inner: Arc<FixtureRecorder>,
        entered: Arc<AtomicBool>,
    }

    impl AudioRecorder for FlaggingRecorder {
        fn start(
            &self,
            session_id: SessionId,
            context: Arc<DictationContext>,
            consumer: Arc<dyn AudioConsumer>,
            progress: Arc<dyn RecordingProgressSink>,
        ) -> BoxFuture<'static, Result<Box<dyn ActiveRecording>, BackendError>> {
            self.entered.store(true, Ordering::SeqCst);
            self.inner.start(session_id, context, consumer, progress)
        }
    }

    #[tokio::test]
    async fn a_slow_screen_ocr_never_delays_the_recorder_start() {
        // 合同验收：OCR 慢（槽位永不 Ready）时，engine.start 不得等待它——判据是
        // recorder.start 被进入 + start 在 500ms 时限内返回，且 start 阶段一次
        // 都不探测槽位。
        let entered = Arc::new(AtomicBool::new(false));
        let stops = Arc::new(AtomicUsize::new(0));
        let archive = Arc::new(FixtureArchive {
            available: AtomicBool::new(true),
            discards: Arc::new(AtomicUsize::new(0)),
            pcm: Vec::new(),
        });
        let fixture = fixture_engine(
            false,
            Ok(crate::ports::PolishOutput::text("polished")),
            None,
            None,
        );
        let engine = PipelineDictationEngine::new(
            Arc::new(FlaggingRecorder {
                inner: Arc::new(FixtureRecorder {
                    stops: Arc::clone(&stops),
                    archive: Arc::clone(&archive),
                    fail: false,
                }),
                entered: Arc::clone(&entered),
            }),
            Arc::new(FixtureTranscriber {
                session: Arc::new(FixtureTranscriptionSession {
                    text: "raw text".to_string(),
                    pcm: Arc::new(Mutex::new(Vec::new())),
                    cancels: Arc::new(AtomicUsize::new(0)),
                    finishes: Arc::new(AtomicUsize::new(0)),
                    finish_entered: None,
                    finish_release: None,
                }),
            }),
            Arc::new(FixturePolisher {
                result: Ok(crate::ports::PolishOutput::text("polished")),
                calls: Arc::new(AtomicUsize::new(0)),
                cancels: Arc::new(AtomicUsize::new(0)),
                contexts: Arc::new(Mutex::new(Vec::new())),
            }),
        );
        let _ = fixture;
        let slot = FakeScreenOcrSlot::pending();
        let session_id = SessionId::new();
        let progress: Arc<RecordingProgress> = Arc::new(RecordingProgress::default());
        let sink: Arc<dyn EngineProgressSink> =
            Arc::clone(&progress) as Arc<dyn EngineProgressSink>;
        let started = std::time::Instant::now();
        tokio::time::timeout(
            std::time::Duration::from_millis(500),
            engine.start(
                session_id,
                context_with_screen_slot(Arc::clone(&slot) as Arc<dyn crate::ports::ScreenOcrSlot>),
                Arc::clone(&sink),
            ),
        )
        .await
        .expect("recorder start must not wait for screen OCR")
        .unwrap();

        assert!(entered.load(Ordering::SeqCst), "recorder.start 已进入");
        assert_eq!(
            slot.takes.load(Ordering::SeqCst),
            0,
            "start 阶段绝不探测槽位"
        );
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
        let _ = engine.finish(session_id, sink).await;
    }

    fn context_with_screen_slot(
        slot: Arc<dyn crate::ports::ScreenOcrSlot>,
    ) -> Arc<DictationContext> {
        let mut context = DictationContext::default();
        context.screen_ocr_pending = crate::screen_ocr::ScreenOcrPending::new(slot);
        Arc::new(context)
    }

    #[tokio::test]
    async fn finish_freezes_ready_screen_ocr_into_the_polish_context() {
        let fixture = fixture_engine(
            false,
            Ok(crate::ports::PolishOutput::text("polished")),
            None,
            None,
        );
        let session_id = SessionId::new();
        fixture
            .engine
            .start(
                session_id,
                context_with_screen_slot(FakeScreenOcrSlot::with_ready(
                    "设置\n设置\n声音与屏幕\nSound & Screen",
                )),
                fixture.progress.clone(),
            )
            .await
            .unwrap();
        fixture
            .engine
            .finish(session_id, fixture.progress.clone())
            .await
            .unwrap();

        let contexts = fixture.polish_contexts.lock().unwrap();
        let polish = &contexts[0].polish;
        assert_eq!(
            polish.screen_ocr.as_deref(),
            Some("设置\n声音与屏幕\nSound & Screen"),
            "就绪结果进独立字段，且经 normalize 折叠连续重复行"
        );
        assert_eq!(
            polish.screen_ocr_trace.status,
            crate::polish_diagnostics::PolishContextStatus::Included
        );
    }

    #[tokio::test]
    async fn finish_ignores_pending_screen_ocr_without_waiting_for_it() {
        let fixture = fixture_engine(
            false,
            Ok(crate::ports::PolishOutput::text("polished")),
            None,
            None,
        );
        let session_id = SessionId::new();
        let slot = FakeScreenOcrSlot::pending();
        fixture
            .engine
            .start(
                session_id,
                context_with_screen_slot({
                    let dyn_slot: Arc<dyn crate::ports::ScreenOcrSlot> = {
                        let cloned: Arc<FakeScreenOcrSlot> = Arc::clone(&slot);
                        cloned
                    };
                    dyn_slot
                }),
                fixture.progress.clone(),
            )
            .await
            .unwrap();
        // 若 finish 在这里等待槽位，超时会让本测试失败——非阻塞是合同要求。
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            fixture.engine.finish(session_id, fixture.progress.clone()),
        )
        .await
        .expect("finish must not wait for pending screen OCR")
        .unwrap();

        assert_eq!(fixture.polish_calls.load(Ordering::SeqCst), 1);
        let contexts = fixture.polish_contexts.lock().unwrap();
        assert!(
            contexts[0].polish.screen_ocr.is_none(),
            "未就绪 → 忽略，不进 prompt"
        );
        // 未就绪的轻量结局写进轨迹：诊断显示 notReady，而不是静默的 None。
        assert_eq!(
            contexts[0].polish.screen_ocr_trace.status,
            crate::polish_diagnostics::PolishContextStatus::NotReady
        );
        assert_eq!(slot.takes.load(Ordering::SeqCst), 1, "只探测一次");
        assert_eq!(result.polished_text, "polished");
    }

    #[tokio::test]
    async fn raw_passthrough_never_probes_the_screen_ocr_slot() {
        let fixture = fixture_engine(
            false,
            Ok(crate::ports::PolishOutput::text("polished")),
            None,
            None,
        );
        let session_id = SessionId::new();
        let slot = FakeScreenOcrSlot::with_ready("should not be read");
        let mut context = DictationContext::default();
        let cloned: Arc<FakeScreenOcrSlot> = Arc::clone(&slot);
        let dyn_slot: Arc<dyn crate::ports::ScreenOcrSlot> = cloned;
        context.screen_ocr_pending = crate::screen_ocr::ScreenOcrPending::new(dyn_slot);
        fixture
            .engine
            .start(session_id, Arc::new(context), fixture.progress.clone())
            .await
            .unwrap();
        fixture
            .engine
            .finish(session_id, fixture.progress.clone())
            .await
            .unwrap();

        assert_eq!(
            fixture.polish_calls.load(Ordering::SeqCst),
            1,
            "legacy Raw uses the fixed polisher"
        );
        assert_eq!(
            slot.takes.load(Ordering::SeqCst),
            1,
            "the fixed polisher freezes OCR exactly once"
        );
    }
}
