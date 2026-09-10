//! 润色调用诊断 —— 随听写历史记录保存的「这次实际发生了什么」快照。
//!
//! 用户已明确授权（2026-09-08）：完整最终 prompt（含 OCR / 光标上下文正文）
//! 随**本机** history.json 保存，用于诊断「模型到底看到了什么」。边界：
//! - 截图仍然不落盘；诊断不含任何图像数据；
//! - 不上传、不进普通日志 —— 本模块所有含正文的类型手写 Debug 脱敏
//!   （派生 Debug 可能经 `{:?}` 进日志，见各 impl）；
//! - 快照必须在真正渲染 prompt 的地方（local_llm）用喂给 tokenizer 的同一份值
//!   构建，历史层不得事后重拼。
//!
//! 旧历史记录没有 `polishDiagnostics` 字段：serde default 反序列化为 `None`，
//! UI 显示「未保存诊断」。

use serde::{Deserialize, Serialize};

use crate::dictation_context::DictationContext;

/// 一条实际渲染进最终 prompt 的结构化消息（含真实 prior turns）。
///
/// Debug 脱敏：只打印 role 与内容长度，不打印正文。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolishDiagnosticMessage {
    pub role: String,
    pub content: String,
}

impl std::fmt::Debug for PolishDiagnosticMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolishDiagnosticMessage")
            .field("role", &self.role)
            .field("contentChars", &self.content.chars().count())
            .finish()
    }
}

/// 上下文（光标文档 / 屏幕 OCR）在本次润色调用中的结局。
///
/// 这是轻量枚举，可安全派生 Debug（不含正文）。默认值 `Unknown` 供轨迹兜底。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PolishContextStatus {
    /// 正文真正进入了最终 prompt。
    Included,
    /// 捕获到了正文，但本次调用没有把它渲染进任何 prompt（模型短路 / 未调用）。
    NotAdopted,
    /// 偏好开关关闭。
    Disabled,
    /// 宿主平台没有该能力。
    NotSupported,
    /// 观测启动即失败（无前台应用 / 自身前台 / Secure Input / 敏感应用 / 窗口列表 / spawn）。
    NotStarted,
    /// 权限未授予（屏幕录制 / 辅助功能）。
    PermissionDenied,
    /// 敏感应用、密码框或 Secure Input 拦截。
    SensitiveBlocked,
    /// 冻结目标（前台 app / 窗口 / 焦点）在截图或读取前变了。
    TargetChanged,
    /// 观测或读取超时。
    Timeout,
    /// 润色输入冻结点仍未就绪（不等待，late 结果作废）。
    NotReady,
    /// 拿到了结果但规范化后为空。
    Empty,
    /// 其它失败（reason 提供机器可读细节）。
    Failed,
    /// 读取被安全闸门拦下（光标侧 probe status = blocked）。
    Blocked,
    /// AX 可达但拿不到文档（无焦点 / 控件不支持 / 权限缺失）。
    Unavailable,
    /// 宿主没有报告结局（如测试用 Noop 适配器）。
    #[default]
    Unknown,
}

/// 单个上下文的诊断条目：结局 + 机器可读原因 + （Included/NotAdopted 时）真正
/// 进入或本可进入 prompt 的处理后正文。
///
/// Debug 脱敏：不打印 text 正文，只打印长度。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolishContextDiagnostics {
    pub status: PolishContextStatus,
    pub reason: Option<String>,
    pub text: Option<String>,
}

impl std::fmt::Debug for PolishContextDiagnostics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolishContextDiagnostics")
            .field("status", &self.status)
            .field("reason", &self.reason)
            .field("textChars", &self.text.as_ref().map(|t| t.chars().count()))
            .finish()
    }
}

/// 润色调用推进到的最远阶段（失败时 = 失败发生前到达的阶段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PolishDiagnosticsStage {
    /// 润色从未进入 local_llm（Raw 直通 / ASR 先失败 / 启动时 LLM 不可用）。
    NotCalled,
    /// 固定模型文件缺失。
    ModelMissing,
    /// 模型或 backend 载入失败。
    ModelLoadFailed,
    /// chat template 获取或渲染失败。
    RenderFailed,
    /// messages 与最终 prompt 已产出（此后 prompt/messages 字段有效）。
    Rendered,
    /// tokenizer 完成，token 数已知（超上下文上限在此失败）。
    Tokenized,
    /// prefill decode 成功。
    Prefilled,
    /// 采样生成中（budget 耗尽在此失败）。
    Generating,
    /// 生成完成（EOG 停止）。
    Succeeded,
}

/// 调用结局。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PolishDiagnosticsOutcome {
    Succeeded,
    Failed,
    NotCalled,
}

/// prompt 渲染路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PolishTemplateKind {
    /// Gemma 4 canonical chat template（Rust 侧自渲染，enable_thinking=false）。
    Gemma4Canonical,
    /// GGUF 元数据模板经 llama.cpp `apply_chat_template` 渲染（遗留路径）。
    GgufChatTemplate,
}

/// 失败时的脱敏错误摘要：code + message 均不得包含 prompt 正文
/// （local_llm 的错误信息只含计数与状态词）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolishDiagnosticError {
    pub code: String,
    pub message: String,
}

/// 一次润色调用的完整诊断快照，内嵌在 history.json 的条目里。
///
/// Debug 脱敏：不打印 prompt / messages / 上下文正文（长度代替），因为
/// DictationSession 派生 Debug，其 `{:?}` 输出可能进日志。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolishDiagnostics {
    pub stage: PolishDiagnosticsStage,
    pub outcome: PolishDiagnosticsOutcome,
    pub provider: Option<String>,
    /// 模型真实文件名（如 gemma-4-E2B-it-Q4_0.gguf）。
    pub model: Option<String>,
    pub template: Option<PolishTemplateKind>,
    /// tokenizer 是否注入 BOS（AddBos::Always）。
    pub add_bos: Option<bool>,
    /// BOS 处理说明（自描述记录，历史可独立解读）。
    pub add_bos_note: Option<String>,
    /// 完整最终 rendered prompt（未渲染时 None）。与喂给 tokenizer 的是同一份值。
    pub prompt: Option<String>,
    /// 结构化消息（含实际 prior turns）。与渲染 prompt 的是同一份值。
    pub messages: Option<Vec<PolishDiagnosticMessage>>,
    pub prompt_tokens: Option<usize>,
    pub context_tokens: Option<u32>,
    pub generation_budget: Option<usize>,
    pub error: Option<PolishDiagnosticError>,
    /// NotCalled 时的原因标识（机器可读短串）。
    pub not_called_reason: Option<String>,
    pub cursor: Option<PolishContextDiagnostics>,
    pub screen_ocr: Option<PolishContextDiagnostics>,
}

impl std::fmt::Debug for PolishDiagnostics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolishDiagnostics")
            .field("stage", &self.stage)
            .field("outcome", &self.outcome)
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("template", &self.template)
            .field("addBos", &self.add_bos)
            .field(
                "promptChars",
                &self.prompt.as_ref().map(|p| p.chars().count()),
            )
            .field("messageCount", &self.messages.as_ref().map(|m| m.len()))
            .field("promptTokens", &self.prompt_tokens)
            .field("contextTokens", &self.context_tokens)
            .field("generationBudget", &self.generation_budget)
            .field("error", &self.error)
            .field("notCalledReason", &self.not_called_reason)
            .field("cursor", &self.cursor)
            .field("screenOcr", &self.screen_ocr)
            .finish()
    }
}

/// prompt 渲染前的轻量上下文轨迹，挂在 [`crate::dictation_context::DictationContext`]
/// 上随冻结副本流转。不序列化、不含正文。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PolishContextTrace {
    pub status: PolishContextStatus,
    pub reason: Option<String>,
}

impl PolishContextTrace {
    pub fn new(status: PolishContextStatus, reason: Option<String>) -> Self {
        Self { status, reason }
    }

    pub fn included() -> Self {
        Self::new(PolishContextStatus::Included, None)
    }
}

/// AddBos 的固定说明（llama.cpp tokenizer 层注入 BOS，正文不含 `<bos>` 文本）。
pub const ADD_BOS_NOTE: &str =
    "BOS token is injected by the llama.cpp tokenizer via AddBos::Always; the rendered prompt text itself never contains <bos>.";

/// 把轨迹 + 捕获正文折叠成持久化的上下文诊断条目。
///
/// `adopted` = 最终 prompt 真的渲染出来了（上下文进入了 prompt）。捕获到正文但
/// 没渲染 prompt 时显示 `NotAdopted`，不得误标 `Included`。
pub fn context_diagnostics(
    trace: &PolishContextTrace,
    text: Option<&str>,
    adopted: bool,
) -> PolishContextDiagnostics {
    match (text, adopted) {
        (Some(text), true) => PolishContextDiagnostics {
            status: PolishContextStatus::Included,
            reason: trace.reason.clone(),
            text: Some(text.to_string()),
        },
        (Some(text), false) => PolishContextDiagnostics {
            status: PolishContextStatus::NotAdopted,
            reason: trace.reason.clone(),
            text: Some(text.to_string()),
        },
        (None, _) => PolishContextDiagnostics {
            status: trace.status,
            reason: trace.reason.clone(),
            text: None,
        },
    }
}

impl PolishDiagnostics {
    /// 「未调用模型」的诊断快照：Raw 直通 / ASR 先失败 / 启动时 LLM 不可用等。
    /// 没有可重拼的 prompt —— prompt 字段保持 None，绝不事后伪造。
    pub fn not_called_from_context(context: &DictationContext, reason: &str) -> Self {
        Self {
            stage: PolishDiagnosticsStage::NotCalled,
            outcome: PolishDiagnosticsOutcome::NotCalled,
            provider: Some(context.llm.provider_type.clone()),
            model: context.llm.model.clone(),
            template: None,
            add_bos: None,
            add_bos_note: None,
            prompt: None,
            messages: None,
            prompt_tokens: None,
            context_tokens: None,
            generation_budget: None,
            error: None,
            not_called_reason: Some(reason.to_string()),
            cursor: Some(context_diagnostics(
                &context.polish.cursor_context_trace,
                context.polish.cursor_context.as_deref(),
                false,
            )),
            screen_ocr: Some(context_diagnostics(
                &context.polish.screen_ocr_trace,
                context.polish.screen_ocr.as_deref(),
                false,
            )),
        }
    }
}

use crate::ports::{ScreenOcrNotStarted, ScreenOcrOutcome};

/// 槽位终态原因 → 轨迹。`None`（仍在识别中 / 无记录）映射为 NotReady。
pub fn screen_ocr_outcome_trace(outcome: Option<ScreenOcrOutcome>) -> PolishContextTrace {
    match outcome {
        None => PolishContextTrace::new(
            PolishContextStatus::NotReady,
            Some("observation not ready at the freeze point".to_string()),
        ),
        Some(ScreenOcrOutcome::Timeout) => PolishContextTrace::new(
            PolishContextStatus::Timeout,
            Some("observation deadline reached".to_string()),
        ),
        Some(ScreenOcrOutcome::TargetChanged) => PolishContextTrace::new(
            PolishContextStatus::TargetChanged,
            Some("frozen target changed before capture".to_string()),
        ),
        Some(ScreenOcrOutcome::PermissionDenied) => PolishContextTrace::new(
            PolishContextStatus::PermissionDenied,
            Some("screen recording permission not granted".to_string()),
        ),
        Some(ScreenOcrOutcome::SensitiveBlocked) => PolishContextTrace::new(
            PolishContextStatus::SensitiveBlocked,
            Some("focus gate rejected the frozen target".to_string()),
        ),
        Some(ScreenOcrOutcome::Failed) => PolishContextTrace::new(
            PolishContextStatus::Failed,
            Some("observation failed".to_string()),
        ),
        Some(ScreenOcrOutcome::Discarded) => PolishContextTrace::new(
            PolishContextStatus::Failed,
            Some("observation discarded".to_string()),
        ),
    }
}

/// 观测启动即失败 → 轨迹（启动失败也要有状态，不能静默成 Unknown）。
pub fn screen_ocr_not_started_trace(reason: ScreenOcrNotStarted) -> PolishContextTrace {
    PolishContextTrace::new(
        PolishContextStatus::NotStarted,
        Some(reason.as_str().to_string()),
    )
}

/// 观测已启动、仍在识别 → 轨迹（冻结点若仍未就绪会由 outcome 覆盖）。
pub fn screen_ocr_pending_trace() -> PolishContextTrace {
    PolishContextTrace::new(
        PolishContextStatus::NotReady,
        Some("observation running".to_string()),
    )
}

/// 宿主光标 probe 的 status 字符串（HostContextCapture.cursor_status，取值
/// ok/blocked/unavailable/timeout/unsupported）→ 轨迹。`status=None` 或未知值
/// （宿主未报告，如测试 Noop 适配器）映射为 Unknown。
pub fn cursor_trace_from_capture(
    status: Option<&str>,
    reason: Option<String>,
    has_text: bool,
) -> PolishContextTrace {
    let reason_or = |fallback: &'static str, reason: Option<String>| {
        reason
            .filter(|value| !value.trim().is_empty())
            .or(Some(fallback.to_string()))
    };
    match status {
        Some("ok") => {
            if has_text {
                PolishContextTrace::included()
            } else {
                PolishContextTrace::new(
                    PolishContextStatus::Empty,
                    Some("probe ok but no document text".to_string()),
                )
            }
        }
        Some("blocked") => PolishContextTrace::new(
            PolishContextStatus::SensitiveBlocked,
            reason_or("blocked by the safety gate", reason),
        ),
        Some("unavailable") => PolishContextTrace::new(
            PolishContextStatus::Unavailable,
            reason_or("document read unavailable", reason),
        ),
        Some("timeout") => PolishContextTrace::new(
            PolishContextStatus::Timeout,
            reason_or("document read timed out", reason),
        ),
        Some("unsupported") => PolishContextTrace::new(
            PolishContextStatus::NotSupported,
            reason_or("cursor context unsupported on this platform", reason),
        ),
        other => PolishContextTrace::new(
            PolishContextStatus::Unknown,
            Some(format!(
                "host did not report a cursor probe status{}",
                other
                    .map(|value| format!(" (reported: {value})"))
                    .unwrap_or_default()
            )),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_contains_prompt_or_message_content() {
        let diagnostics = PolishDiagnostics {
            stage: PolishDiagnosticsStage::Succeeded,
            outcome: PolishDiagnosticsOutcome::Succeeded,
            provider: Some("local-llama".into()),
            model: Some("gemma-4-E2B-it-Q4_0.gguf".into()),
            template: Some(PolishTemplateKind::Gemma4Canonical),
            add_bos: Some(true),
            add_bos_note: Some(ADD_BOS_NOTE.into()),
            prompt: Some("SECRET PROMPT BODY 用户机密".into()),
            messages: Some(vec![PolishDiagnosticMessage {
                role: "system".into(),
                content: "SECRET SYSTEM MESSAGE".into(),
            }]),
            prompt_tokens: Some(128),
            context_tokens: Some(16_384),
            generation_budget: Some(1024),
            error: None,
            not_called_reason: None,
            cursor: Some(PolishContextDiagnostics {
                status: PolishContextStatus::Included,
                reason: None,
                text: Some("SECRET CURSOR TEXT".into()),
            }),
            screen_ocr: Some(PolishContextDiagnostics {
                status: PolishContextStatus::Included,
                reason: None,
                text: Some("SECRET OCR TEXT".into()),
            }),
        };
        let rendered = format!("{diagnostics:?}");
        for secret in [
            "SECRET PROMPT BODY",
            "SECRET SYSTEM MESSAGE",
            "SECRET CURSOR TEXT",
            "SECRET OCR TEXT",
            "用户机密",
        ] {
            assert!(
                !rendered.contains(secret),
                "Debug output leaked {secret:?}: {rendered}"
            );
        }
        // 元数据仍可见（排障需要）。
        assert!(rendered.contains("Succeeded"));
        assert!(rendered.contains("gemma-4-E2B-it-Q4_0.gguf"));
        assert!(rendered.contains("promptChars: Some(23)"));
        assert!(rendered.contains("textChars: Some(18)"));
    }

    #[test]
    fn context_diagnostics_distinguishes_included_from_not_adopted() {
        let trace = PolishContextTrace::included();
        let included = context_diagnostics(&trace, Some("ctx"), true);
        assert_eq!(included.status, PolishContextStatus::Included);
        assert_eq!(included.text.as_deref(), Some("ctx"));

        let not_adopted = context_diagnostics(&trace, Some("ctx"), false);
        assert_eq!(not_adopted.status, PolishContextStatus::NotAdopted);
        assert_eq!(not_adopted.text.as_deref(), Some("ctx"));

        let missing = context_diagnostics(
            &PolishContextTrace::new(PolishContextStatus::NotReady, None),
            None,
            true,
        );
        assert_eq!(missing.status, PolishContextStatus::NotReady);
        assert_eq!(missing.text, None);
    }

    #[test]
    fn old_history_json_without_the_field_deserializes_to_none() {
        #[derive(serde::Deserialize)]
        struct Row {
            #[serde(default)]
            polish_diagnostics: Option<PolishDiagnostics>,
        }
        let row: Row = serde_json::from_str("{}").expect("old rows must stay readable");
        assert!(row.polish_diagnostics.is_none());
    }
}
