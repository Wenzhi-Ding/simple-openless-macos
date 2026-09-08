//! ASR 模块入口。
//!
//! 减法改造(纯本地):云端流式 ASR(volcengine/whisper/bailian 等)已移除,
//! 只保留本地引擎(`local/`)与共享 PCM/WAV 工具。

pub mod local;

pub use openless_core::asr::{pcm, wav, RawTranscript};
