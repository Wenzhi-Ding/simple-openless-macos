//! Immutable configuration captured for one dictation session.
//!
//! Hosts and provider adapters receive this snapshot instead of re-reading
//! mutable preferences while recording, transcribing, polishing or inserting.

use crate::polish_diagnostics::{
    screen_ocr_outcome_trace, PolishContextStatus, PolishContextTrace,
};
use crate::screen_ocr::ScreenOcrPending;
use crate::shared_types::UserPreferences;
use crate::types::DictationSession;

pub const ASR_PROMPT_CHAR_BUDGET: usize = 240;

/// 短输入跳润色的门槛：润色输入（ASR 结果经词表同音纠正之后）的非空白字符数
/// 不超过该值时，听写管线不调用 Gemma，直接按 raw 插入。
/// 依据：两三个字的听写跑满一次生成（载入、prefill、采样）没有收益；
/// 用户 2026-11-02 批准固定为 4，不新增用户设置项。
/// 只作用于听写管线；历史页「重新润色」直接调用润色器，不受此限。
pub const POLISH_SKIP_MAX_CHARS: usize = 4;

/// 润色输入的有效长度口径：去掉空白字符后的 Unicode 标量个数。
/// 判定与诊断原因共用这一处，避免两处口径漂移。
pub fn non_whitespace_char_count(text: &str) -> usize {
    text.chars().filter(|c| !c.is_whitespace()).count()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DictationAudioSource {
    #[default]
    Microphone,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictationStartOptions {
    pub audio_source: DictationAudioSource,
    pub insert_text: bool,
    pub front_app: Option<String>,
    pub cursor_context: Option<String>,
}

impl Default for DictationStartOptions {
    fn default() -> Self {
        Self {
            audio_source: DictationAudioSource::Microphone,
            insert_text: true,
            front_app: None,
            cursor_context: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInvocation {
    /// Stable channel identifier used to read channel-scoped credentials.
    /// This is deliberately distinct from `provider_type`: users may configure
    /// multiple channels backed by the same provider protocol.
    pub provider_id: String,
    /// Protocol/implementation routing key such as `volcengine`, `deepseek`,
    /// or `openai-compatible`.
    pub provider_type: String,
    pub model: Option<String>,
    pub language: Option<String>,
    pub prompt: Option<String>,
    /// Optional native runtime detail frozen with the session.
    pub runtime: Option<String>,
    /// Native model retention policy captured at session start.
    pub keep_loaded_secs: Option<u32>,
}

pub(crate) struct DictationProviderInvocations {
    pub asr: ProviderInvocation,
    pub llm: ProviderInvocation,
}

impl DictationProviderInvocations {
    pub(crate) fn new(asr: ProviderInvocation, llm: ProviderInvocation) -> Self {
        Self { asr, llm }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictationPolishContext {
    pub hotwords: Vec<String>,
    pub working_languages: Vec<String>,
    pub llm_thinking_enabled: bool,
    pub context_window_minutes: u32,
    pub front_app: Option<String>,
    pub cursor_context: Option<String>,
    /// 听写触发那一刻前台窗口的本地 OCR 结果（润色输入冻结时从待定槽位非阻塞收取）。
    /// 独立于 cursor_context 的另一份不可信背景参考；默认关闭、无结果时为 None。
    pub screen_ocr: Option<String>,
    /// 光标上下文的轻量捕获结局（不含正文；正文在 `cursor_context`）。
    pub cursor_context_trace: PolishContextTrace,
    /// 屏幕 OCR 的轻量结局（启动失败 / 冻结点未就绪 / 各类拒绝原因）。
    pub screen_ocr_trace: PolishContextTrace,
    /// Newest-first turns captured when the session starts.
    pub prior_turns: Vec<PolishHistoryTurn>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolishHistoryTurn {
    pub raw_text: String,
    pub polished_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictationInsertionContext {
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingPlan {
    pub microphone_device_name: Option<String>,
    pub mute_during_recording: bool,
    /// Whether the Host may create an audio archive at all. QA/Selection Voice
    /// keep PCM in memory; successful-recording retention is a separate policy.
    pub archive_enabled: bool,
    pub archive_successful_recording: bool,
    pub retention_days: u32,
    pub max_entries: Option<u32>,
    pub silence_after_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictationContext {
    pub audio_source: DictationAudioSource,
    pub recording: RecordingPlan,
    pub asr: ProviderInvocation,
    pub llm: ProviderInvocation,
    /// Raw may start without an LLM. Preserve the start-time resolution
    /// failure instead of using a disabled channel or silently selecting a
    /// newly configured provider.
    pub deferred_llm_error: Option<crate::errors::BackendError>,
    pub polish: DictationPolishContext,
    /// 本会话的屏幕 OCR 待定槽位（触发时冻结目标、后台识别）。
    pub screen_ocr_pending: ScreenOcrPending,
    pub insertion: DictationInsertionContext,
}

impl Default for DictationContext {
    fn default() -> Self {
        let preferences = UserPreferences::default();
        Self::capture(
            &preferences,
            DictationProviderInvocations::new(
                ProviderInvocation::for_provider(preferences.active_asr_provider.clone()),
                ProviderInvocation::for_provider(preferences.active_llm_provider.clone()),
            ),
            Vec::new(),
            Vec::new(),
            &DictationStartOptions::default(),
        )
    }
}

impl DictationContext {
    pub(crate) fn capture(
        preferences: &UserPreferences,
        providers: DictationProviderInvocations,
        hotwords: Vec<String>,
        recent_history: Vec<DictationSession>,
        options: &DictationStartOptions,
    ) -> Self {
        let DictationProviderInvocations { mut asr, llm } = providers;
        let hotwords = normalize_hotwords(hotwords);
        let (fallback_asr_model, fallback_asr_language) =
            selected_asr_details(preferences, &asr.provider_type);
        if asr.model.is_none() {
            asr.model = fallback_asr_model;
        }
        if asr.language.is_none() {
            asr.language = fallback_asr_language;
        }
        asr.keep_loaded_secs = Some(preferences.local_asr_keep_loaded_secs);
        asr.prompt = build_asr_prompt(&hotwords);
        let prior_turns = eligible_polish_context_turns(recent_history);
        Self {
            audio_source: options.audio_source,
            recording: RecordingPlan {
                microphone_device_name: non_blank(&preferences.microphone_device_name),
                mute_during_recording: preferences.mute_during_recording,
                archive_enabled: true,
                archive_successful_recording: preferences.record_audio_for_debug,
                retention_days: preferences.history_retention_days,
                // Recordings and transcript history have independent caps in
                // the UI; only the age limit is shared with history.
                max_entries: preferences.audio_recording_max_entries,
                silence_after_ms: preferences.silence_auto_stop_enabled
                    .then(|| (preferences.silence_auto_stop_seconds * 1_000.0).round() as u64),
            },
            asr,
            llm,
            deferred_llm_error: None,
            polish: DictationPolishContext {
                hotwords,
                working_languages: preferences.working_languages.clone(),
                llm_thinking_enabled: preferences.llm_thinking_enabled,
                context_window_minutes: preferences.polish_context_window_minutes,
                front_app: options.front_app.clone().and_then(non_blank_owned),
                cursor_context: options.cursor_context.clone().and_then(non_blank_owned),
                // 默认轨迹只表达「core 自身视角」：调用方带正文 = 已捕获候选；
                // 没带 = 未知。api 的 capture_dictation_context 随后会用宿主
                // probe 的真实 status/reason 覆盖（见 cursor_trace_from_capture）。
                cursor_context_trace: PolishContextTrace {
                    status: if options
                        .cursor_context
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
                    {
                        PolishContextStatus::Included
                    } else {
                        PolishContextStatus::Unknown
                    },
                    reason: None,
                },
                screen_ocr: None,
                screen_ocr_trace: PolishContextTrace::new(
                    PolishContextStatus::Unknown,
                    Some("screen observation not started".to_string()),
                ),
                prior_turns,
            },
            screen_ocr_pending: ScreenOcrPending::default(),
            insertion: DictationInsertionContext {
                enabled: options.insert_text,
            },
        }
    }

    // Enabled vocabulary frozen at session start; shared by ASR and polish.
    pub fn enabled_hotwords(&self) -> &[String] {
        &self.polish.hotwords
    }

    pub fn effective_polish_prompts(&self, raw_text: &str) -> (String, String) {
        crate::prompt_compose::compose_polish_prompts(
            raw_text,
            &self.polish.hotwords,
            self.polish.cursor_context.as_deref(),
            self.polish.screen_ocr.as_deref(),
            !self.polish.prior_turns.is_empty(),
        )
    }

    /// 润色输入冻结点调用：非阻塞收取屏幕 OCR 结果；未就绪即忽略，绝不等待
    /// （late 结果作废）。返回带上了 `polish.screen_ocr` 与冻结结局轨迹的副本；
    /// pending 槽位是共享句柄，之后的 discard 语义不受这次收取影响。
    pub fn with_screen_ocr_frozen(&self) -> Self {
        let mut updated = self.clone();
        let trace = match self.screen_ocr_pending.take() {
            crate::screen_ocr::ScreenOcrTake::Ready(text) => {
                updated.polish.screen_ocr = Some(text);
                // 焦点降级放行（2026-09-18 用户授权）：正文照常用，但诊断必须
                // 如实标注「焦点校验未执行」，不能伪装成完整校验过的结果。
                if self.screen_ocr_pending.focus_gate_relaxed() {
                    PolishContextTrace::new(
                        PolishContextStatus::Included,
                        Some(
                            "focus check skipped: target app reported no focused element"
                                .to_string(),
                        ),
                    )
                } else {
                    PolishContextTrace::included()
                }
            }
            crate::screen_ocr::ScreenOcrTake::EmptyNormalized => PolishContextTrace::new(
                PolishContextStatus::Empty,
                Some("ocr text normalized to empty".to_string()),
            ),
            crate::screen_ocr::ScreenOcrTake::NotReady(outcome) => {
                screen_ocr_outcome_trace(outcome)
            }
        };
        updated.polish.screen_ocr_trace = trace;
        updated
    }

    pub fn has_pending_screen_ocr(&self) -> bool {
        self.screen_ocr_pending.is_active()
    }

    /// 作废挂起的屏幕 OCR 观测（cancel / 会话失败 / 被新会话取代）。
    pub fn discard_pending_screen_ocr(&self) {
        self.screen_ocr_pending.discard();
    }

    /// Build the exact system prompt captured for this session.
    pub fn effective_polish_system_prompt(&self) -> String {
        self.effective_polish_prompts("").0
    }

    /// The fixed local polisher always runs for non-short dictation: dictation
    /// never bypasses the LLM by preference. Short input is handled separately
    /// by [`DictationContext::skips_polish_for_short_text`].
    pub fn uses_llm_polisher(&self) -> bool {
        true
    }

    /// 短输入跳润色判定：非空白字符数 ≤ [`POLISH_SKIP_MAX_CHARS`] 时为真。
    /// 调用方必须在拿到 ASR 结果（且词表同音纠正已应用）之后调用；
    /// 跳过后走 raw 插入路径，并在 polishDiagnostics 记 NotCalled 与原因。
    pub fn skips_polish_for_short_text(&self, transcript: &str) -> bool {
        non_whitespace_char_count(transcript) <= POLISH_SKIP_MAX_CHARS
    }
}

impl ProviderInvocation {
    pub fn new(provider_id: impl Into<String>, provider_type: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            provider_type: provider_type.into(),
            model: None,
            language: None,
            prompt: None,
            runtime: None,
            keep_loaded_secs: None,
        }
    }

    pub fn for_provider(provider_id: impl Into<String>) -> Self {
        let provider_id = provider_id.into();
        Self::new(provider_id.clone(), provider_id)
    }
}

pub fn build_asr_prompt(phrases: &[String]) -> Option<String> {
    let mut included = Vec::new();
    let mut used_chars = 0_usize;
    for phrase in phrases {
        let phrase = phrase.trim();
        if phrase.is_empty() {
            continue;
        }
        let added = phrase.chars().count() + usize::from(!included.is_empty()) * 2;
        if used_chars + added + 1 > ASR_PROMPT_CHAR_BUDGET {
            continue;
        }
        included.push(phrase);
        used_chars += added;
    }
    if included.is_empty() {
        return None;
    }
    Some(format!("{}.", included.join(", ")))
}

/// Keep recent successful turns independent of obsolete style labels. The input and
/// output are newest-first so providers can reverse them when building chat
/// messages.
pub fn eligible_polish_context_turns(sessions: Vec<DictationSession>) -> Vec<PolishHistoryTurn> {
    const MAX_POLISH_CONTEXT_TURNS: usize = 2;

    sessions
        .into_iter()
        .filter(|session| session.error_code.is_none() && !session.final_text.trim().is_empty())
        .map(|session| PolishHistoryTurn {
            raw_text: session.raw_transcript,
            polished_text: session.final_text,
        })
        .take(MAX_POLISH_CONTEXT_TURNS)
        .collect()
}

fn selected_asr_details(
    preferences: &UserPreferences,
    _provider_type: &str,
) -> (Option<String>, Option<String>) {
    (non_blank(&preferences.local_asr_active_model), None)
}

fn normalize_hotwords(hotwords: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for hotword in hotwords {
        let hotword = hotword.trim();
        if !hotword.is_empty() && !normalized.iter().any(|current| current == hotword) {
            normalized.push(hotword.to_string());
        }
    }
    normalized
}

fn non_blank(value: &str) -> Option<String> {
    non_blank_owned(value.to_string())
}

fn non_blank_owned(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_is_an_owned_snapshot_of_session_preferences() {
        let mut preferences = UserPreferences {
            microphone_device_name: "USB microphone".to_string(),
            active_asr_provider: "local-qwen3".to_string(),
            active_llm_provider: "openai".to_string(),
            local_asr_active_model: "qwen3-asr-1.7b".to_string(),
            history_max_entries: Some(100),
            audio_recording_max_entries: Some(7),
            working_languages: vec!["简体中文".to_string(), "English".to_string()],
            ..UserPreferences::default()
        };
        let context = DictationContext::capture(
            &preferences,
            DictationProviderInvocations::new(
                ProviderInvocation::for_provider(preferences.active_asr_provider.clone()),
                ProviderInvocation::for_provider(preferences.active_llm_provider.clone()),
            ),
            vec!["OpenLess".to_string()],
            Vec::new(),
            &DictationStartOptions::default(),
        );

        preferences.microphone_device_name = "changed".to_string();
        preferences.active_asr_provider = "changed".to_string();
        assert_eq!(
            context.recording.microphone_device_name.as_deref(),
            Some("USB microphone")
        );
        assert_eq!(context.asr.provider_id, "local-qwen3");
        assert_eq!(context.asr.model.as_deref(), Some("qwen3-asr-1.7b"));
        // Audio archives have their own user-visible limit. History retention
        // may be much larger and must not silently override the recording cap.
        assert_eq!(context.recording.max_entries, Some(7));
        let prompt = context.effective_polish_system_prompt();
        assert!(prompt.contains(crate::prompts::FIXED_POLISH_PROMPT));
    }

    #[test]
    fn frozen_hotwords_are_shared_by_asr_and_polish() {
        let preferences = UserPreferences::default();
        let context = DictationContext::capture(
            &preferences,
            DictationProviderInvocations::new(
                ProviderInvocation::for_provider(preferences.active_asr_provider.clone()),
                ProviderInvocation::for_provider(preferences.active_llm_provider.clone()),
            ),
            vec!["OpenLess".into(), "Codex".into()],
            Vec::new(),
            &DictationStartOptions::default(),
        );
        assert_eq!(context.enabled_hotwords(), ["OpenLess", "Codex"]);
        assert!(context.asr.prompt.as_deref().unwrap().contains("OpenLess"));
        let (system_prompt, _) = context.effective_polish_prompts("hello");
        assert!(system_prompt.contains("OpenLess"));
        assert!(system_prompt.contains("Codex"));
    }

    #[test]
    fn asr_prompt_skips_entries_that_do_not_fit_without_dropping_later_ones() {
        let over_budget = "x".repeat(ASR_PROMPT_CHAR_BUDGET);
        let prompt = build_asr_prompt(&[over_budget, "OpenLess".to_string()]).unwrap();
        assert_eq!(prompt, "OpenLess.");
        assert!(prompt.chars().count() <= ASR_PROMPT_CHAR_BUDGET);
    }

    /// 最小槽位桩：正文就绪，降级标记可变。
    struct ReadySlot {
        relaxed: bool,
    }

    impl crate::ports::ScreenOcrSlot for ReadySlot {
        fn try_take(&self) -> Option<String> {
            Some("OCR 正文".to_string())
        }

        fn discard(&self) {}

        fn focus_gate_relaxed(&self) -> bool {
            self.relaxed
        }
    }

    fn context_with_ocr_slot(
        slot: std::sync::Arc<dyn crate::ports::ScreenOcrSlot>,
    ) -> DictationContext {
        let preferences = UserPreferences::default();
        let mut context = DictationContext::capture(
            &preferences,
            DictationProviderInvocations::new(
                ProviderInvocation::for_provider(preferences.active_asr_provider.clone()),
                ProviderInvocation::for_provider(preferences.active_llm_provider.clone()),
            ),
            Vec::new(),
            Vec::new(),
            &DictationStartOptions::default(),
        );
        context.screen_ocr_pending = crate::screen_ocr::ScreenOcrPending::new(slot);
        context
    }

    #[test]
    fn relaxed_screen_ocr_is_adopted_but_honestly_marked() {
        let context = context_with_ocr_slot(std::sync::Arc::new(ReadySlot { relaxed: true }));
        let frozen = context.with_screen_ocr_frozen();
        assert_eq!(frozen.polish.screen_ocr.as_deref(), Some("OCR 正文"));
        assert_eq!(
            frozen.polish.screen_ocr_trace.status,
            PolishContextStatus::Included
        );
        let reason = frozen.polish.screen_ocr_trace.reason.as_deref().unwrap_or("");
        assert!(
            reason.contains("focus check skipped"),
            "降级放行必须在诊断轨迹里如实标注，实际 reason: {reason}"
        );
    }

    #[test]
    fn short_text_polish_skip_counts_only_non_whitespace_characters() {
        let context = DictationContext::default();
        assert_eq!(POLISH_SKIP_MAX_CHARS, 4);

        // 空白不计入：4 个有效字符在门槛上，跳过；第 5 个字符起照常润色。
        assert!(context.skips_polish_for_short_text("确 定 重 试"));
        assert!(context.skips_polish_for_short_text("确定重试"));
        assert!(!context.skips_polish_for_short_text("确定重试好"));
        assert!(!context.skips_polish_for_short_text("a b c d e"));
        // 纯空白与空串都按 0 字符处理。
        assert!(context.skips_polish_for_short_text("   \n\t "));
        assert!(context.skips_polish_for_short_text(""));
        assert_eq!(non_whitespace_char_count("确 定 重 试"), 4);
    }

    #[test]
    fn fully_gated_screen_ocr_trace_carries_no_reason() {
        let context = context_with_ocr_slot(std::sync::Arc::new(ReadySlot { relaxed: false }));
        let frozen = context.with_screen_ocr_frozen();
        assert_eq!(frozen.polish.screen_ocr.as_deref(), Some("OCR 正文"));
        assert_eq!(
            frozen.polish.screen_ocr_trace.status,
            PolishContextStatus::Included
        );
        assert_eq!(
            frozen.polish.screen_ocr_trace.reason, None,
            "完整校验通过的轨迹不得附加降级标注"
        );
    }
}
