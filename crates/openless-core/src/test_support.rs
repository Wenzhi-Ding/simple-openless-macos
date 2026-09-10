//! Private deterministic adapters for Core unit tests.

use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;

use crate::dictation_context::DictationContext;
use crate::errors::BackendError;
use crate::ports::{
    AudioConsumer, TextPolisher, TextStreamChunk, TextStreamSink, TranscriptOutput,
    TranscriptionEngine, TranscriptionSession,
};
use crate::types::SessionId;

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
