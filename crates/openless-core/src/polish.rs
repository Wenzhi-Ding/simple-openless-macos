//! Local prompt/polish plumbing shared by hosts and in-process engines.
//!
//! 减法改造(纯本地):上游的 OpenAI 兼容 / Codex OAuth HTTP 客户端
//! (`OpenAICompatibleLLMProvider` / `CodexOAuthLLMProvider` 及重试、SSE 解码)
//! 已随云端 provider 整体移除。本模块只保留本地润色链路需要的部分:
//! prompt 组装([`crate::prompt_compose`])、输出清洗([`crate::output_cleaning`])
//! 与 [`LlmCallLabel`](进程内 llama 引擎用来给历史记录标记模型快照)。

pub use crate::output_cleaning::*;
pub use crate::prompt_compose::*;

/// 一次 LLM 调用的构建时快照（provider id + 归一化后的模型 id）。polish 链路在
/// **成功构建 provider、即将发起真实调用**时填充；调用方据此决定要不要把
/// llm_* / polish_ms 落进历史——避免“没调用却记了模型”的伪数据（PR #826 review）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmCallLabel {
    pub provider: String,
    pub model: String,
}

pub mod prompts {
    pub use crate::prompts::*;
}
