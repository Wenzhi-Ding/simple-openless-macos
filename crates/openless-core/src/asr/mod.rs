//! Audio/ASR protocol helpers shared by every host.
//!
//! 减法改造(纯本地):上游的云端 ASR 协议实现(volcengine/whisper/xfyun/
//! elevenlabs/bailian/dashscope/mimo/qwen_realtime/stepfun)已整体移除,
//! 这里只保留与网络无关的 PCM/帧/WAV 工具。

pub mod frame;
pub mod pcm;
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
