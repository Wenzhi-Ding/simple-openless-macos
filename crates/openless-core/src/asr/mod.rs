//! Fixed local-ASR protocol and audio helpers.
//! Only network-independent PCM, frame, WAV, and chunk-joining helpers live here.

pub mod frame;
pub mod pcm;
pub mod transcript;
pub mod wav;

pub use crate::ports::AudioConsumer;

/// What a provider yielded after the stream or batch request completed.
pub type RawTranscript = crate::ports::TranscriptOutput;

/// User-defined hotword used to bias providers that expose that capability.
#[derive(Debug, Clone)]
pub struct DictionaryHotword {
    pub phrase: String,
    pub enabled: bool,
}
