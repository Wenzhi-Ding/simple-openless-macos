use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::types::DictationPhase;

/// Suppress switch bounce and accidental press-release-press bursts before they
/// allocate another recorder or ASR session.
const HOTKEY_DEBOUNCE: Duration = Duration::from_millis(250);
/// 会话收尾后的短冷却（#545/#856）。
///
/// 最初是 600ms，要覆盖当时「停止后立刻回到 Idle」的架构下胶囊离场动画
/// （360ms）与三连按误触。现在停止按键到会话收尾之间隔着整段转写+润色+
/// 上屏（秒级），回弹产生的第三次按下落在转写阶段、被阶段门拦截，根本到不了
/// 冷却窗口；胶囊离场与 New/旧 payload 的竞争由 capsule epoch 机制兜底。冷却
/// 只剩一个职责：收尾瞬间的边界竞态。150ms 足够，不再让用户为上屏后的
/// 重启多等近半秒。
const TERMINAL_COOLDOWN: Duration = Duration::from_millis(150);
/// Modifier-only triggers are ambiguous until the host has had enough time to
/// report a companion key. Explicit custom combinations skip this delay.
const MODIFIER_ARBITRATION_GRACE: Duration = Duration::from_millis(150);
/// Native events may overtake the serialized Pressed bridge while session start
/// awaits microphone/ASR setup. Keep a bounded set of those early Combined ids.
const MAX_PENDING_COMBINED: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HotkeyIntent {
    Noop,
    WaitForModifierGrace { press_id: u64 },
    Start { press_id: u64 },
    Stop,
    Cancel { press_id: u64 },
}

#[derive(Debug, Clone, Copy)]
struct Press {
    /// Stable id allocated by the host once per physical down/up cycle.
    id: u64,
    /// False means this press was rejected by debounce/cooldown and its release
    /// must remain a no-op even if a different session is active by then.
    accepted: bool,
}

/// Core-owned interpreter for the main dictation hotkey (toggle only: one
/// press starts, the next press stops).
///
/// The host allocates a `press_id` and forwards timestamped edges. Keeping all
/// policy here gives the macOS event tap and Tauri fallback one generation,
/// arbitration, debounce, and cooldown implementation.
#[derive(Debug, Default)]
pub(crate) struct HotkeyInterpreter {
    /// The latest unmatched physical press. Release must carry the same id.
    held: Option<Press>,
    /// The press generation that actually started the active Core session.
    /// Combined may cancel only this generation, never a later session.
    session_press_id: Option<u64>,
    /// Combined can arrive before Pressed because it uses a separate low-latency
    /// channel. The queue preserves multiple overtaking generations in order.
    pending_combined: VecDeque<u64>,
    /// Last accepted edge that consumed the debounce window. A Combined gesture
    /// clears its own entry because it never represented a dictation request.
    last_dispatch: Option<(u64, Instant)>,
    /// Set whenever a dictation reaches any terminal path and cleared only when
    /// the deadline expires naturally or a same-generation combo is cancelled.
    cooldown_until: Option<Instant>,
}

impl HotkeyInterpreter {
    pub(crate) const MODIFIER_ARBITRATION_GRACE: Duration = MODIFIER_ARBITRATION_GRACE;

    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn press(
        &mut self,
        press_id: u64,
        at: Instant,
        phase: DictationPhase,
        modifier_only: bool,
    ) -> HotkeyIntent {
        // Zero is reserved for an unmatched host edge. Repeated key-down events
        // retain their physical id, so they are ignored without touching state.
        if press_id == 0 || self.held.is_some_and(|press| press.id == press_id) {
            return HotkeyIntent::Noop;
        }
        // A fresh generation supersedes an unmatched stale press. This keeps a
        // dropped native key-up from permanently latching the interpreter.
        self.held = None;
        self.held = Some(Press {
            id: press_id,
            accepted: false,
        });
        if self.take_combined(press_id) {
            // The companion key overtook Pressed on its independent bridge. It
            // must consume this generation before any microphone work starts.
            self.held = None;
            return HotkeyIntent::Noop;
        }
        if self
            .last_dispatch
            .is_some_and(|(_, last)| at.saturating_duration_since(last) < HOTKEY_DEBOUNCE)
        {
            return HotkeyIntent::Noop;
        }
        self.held.as_mut().expect("press was just stored").accepted = true;

        let intent = match phase {
            DictationPhase::Idle
                if self.cooldown_until.is_some_and(|deadline| at < deadline) =>
            {
                HotkeyIntent::Noop
            }
            DictationPhase::Idle => HotkeyIntent::Start { press_id },
            DictationPhase::Starting | DictationPhase::Recording => HotkeyIntent::Stop,
            _ => HotkeyIntent::Noop,
        };
        // 只有真正派发了动作的按下才消耗防抖窗口。被冷却/阶段策略丢弃的按下
        // 不记时间戳——否则用户撞上冷却后再重按，还会被 250ms 防抖再吞一次，
        // 封锁期被感知得比实际更长。
        if matches!(intent, HotkeyIntent::Start { .. } | HotkeyIntent::Stop) {
            self.last_dispatch = Some((press_id, at));
        }
        if matches!(intent, HotkeyIntent::Start { .. }) && modifier_only {
            // Option/Ctrl by itself may be speech, while Option/Ctrl plus a
            // normal key is ordinary typing. The caller waits once and asks us
            // to resolve the same generation through `after_modifier_grace`.
            HotkeyIntent::WaitForModifierGrace { press_id }
        } else {
            if matches!(intent, HotkeyIntent::Start { .. }) {
                self.session_press_id = Some(press_id);
            }
            intent
        }
    }

    pub(crate) fn after_modifier_grace(
        &mut self,
        press_id: u64,
        phase: DictationPhase,
    ) -> HotkeyIntent {
        if self.take_combined(press_id) {
            // Cancelling during grace must not consume the debounce window;
            // the user's next intentional press should work immediately.
            self.clear_press(press_id);
            return HotkeyIntent::Noop;
        }
        if !self
            .held
            .is_some_and(|press| press.id == press_id && press.accepted)
            || phase != DictationPhase::Idle
        {
            return HotkeyIntent::Noop;
        }
        self.session_press_id = Some(press_id);
        HotkeyIntent::Start { press_id }
    }

    pub(crate) fn release(&mut self, press_id: u64) -> HotkeyIntent {
        // Generation matching is the release-side stale-event guard. An old
        // key-up can never stop a session opened by a newer press. Toggle mode
        // acts on the press edge only, so a matched release just clears the
        // latch; the stop itself is decided by the next press.
        let Some(press) = self.held.filter(|press| press.id == press_id) else {
            return HotkeyIntent::Noop;
        };
        self.held = None;
        if !press.accepted || self.take_combined(press_id) {
            return HotkeyIntent::Noop;
        }
        HotkeyIntent::Noop
    }

    pub(crate) fn combined(&mut self, press_id: u64) -> HotkeyIntent {
        if press_id == 0 {
            return HotkeyIntent::Noop;
        }
        if !self.pending_combined.contains(&press_id) {
            self.pending_combined.push_back(press_id);
            if self.pending_combined.len() > MAX_PENDING_COMBINED {
                self.pending_combined.pop_front();
            }
        }
        self.clear_press(press_id);
        // A companion key cancels only when this exact press created the active
        // session. Otherwise the queued marker is consumed by Pressed/grace or
        // eventually evicted as a harmless stale generation.
        if self.session_press_id == Some(press_id) {
            HotkeyIntent::Cancel { press_id }
        } else {
            HotkeyIntent::Noop
        }
    }

    pub(crate) fn start_finished(&mut self, press_id: u64, started: bool) -> bool {
        // Start awaits platform work. Re-check the queue afterwards to close the
        // race where Combined arrived after Start intent but before setup ended.
        let combined = self.take_combined(press_id);
        if (!started || combined) && self.session_press_id == Some(press_id) {
            self.session_press_id = None;
        }
        combined
    }

    pub(crate) fn terminal(&mut self, at: Instant) {
        // Clear generation state on every success/failure/cancel terminal path;
        // the cooldown is deliberately shared across all those outcomes.
        self.held = None;
        self.session_press_id = None;
        self.cooldown_until = Some(at + TERMINAL_COOLDOWN);
    }

    pub(crate) fn combo_cancelled(&mut self, press_id: u64) {
        self.take_combined(press_id);
        self.clear_press(press_id);
        if self.session_press_id == Some(press_id) {
            self.session_press_id = None;
        }
        // A combined keyboard gesture never requested dictation, so it should
        // not penalize the next real press with session cooldown.
        self.cooldown_until = None;
    }

    fn clear_press(&mut self, press_id: u64) {
        if self.held.is_some_and(|press| press.id == press_id) {
            self.held = None;
        }
        if self
            .last_dispatch
            .is_some_and(|(last_press_id, _)| last_press_id == press_id)
        {
            self.last_dispatch = None;
        }
    }

    fn take_combined(&mut self, press_id: u64) -> bool {
        self.pending_combined
            .iter()
            .position(|pending| *pending == press_id)
            .and_then(|index| self.pending_combined.remove(index))
            .is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_before_pressed_cancels_only_that_generation() {
        let start = Instant::now();
        let mut interpreter = HotkeyInterpreter::default();

        assert_eq!(
            interpreter.combined(1),
            HotkeyIntent::Noop,
            "Combined may arrive before Pressed"
        );
        assert_eq!(
            interpreter.press(1, start, DictationPhase::Idle, true),
            HotkeyIntent::Noop
        );
        assert_eq!(
            interpreter.press(2, start + Duration::from_millis(10), DictationPhase::Idle, false),
            HotkeyIntent::Start { press_id: 2 }
        );
    }

    #[test]
    fn modifier_only_waits_for_grace_but_custom_combo_starts_immediately() {
        let start = Instant::now();
        let mut interpreter = HotkeyInterpreter::default();

        assert_eq!(
            interpreter.press(1, start, DictationPhase::Idle, true,),
            HotkeyIntent::WaitForModifierGrace { press_id: 1 }
        );
        assert_eq!(interpreter.combined(1), HotkeyIntent::Noop);
        assert_eq!(
            interpreter.after_modifier_grace(1, DictationPhase::Idle),
            HotkeyIntent::Noop
        );
        assert_eq!(
            interpreter.press(2, start + Duration::from_millis(10), DictationPhase::Idle, false),
            HotkeyIntent::Start { press_id: 2 },
            "an explicit custom combo has no modifier-only ambiguity"
        );
    }

    #[test]
    fn stale_release_generation_is_rejected() {
        let start = Instant::now();
        let mut interpreter = HotkeyInterpreter::default();

        assert_eq!(
            interpreter.press(1, start, DictationPhase::Idle, false),
            HotkeyIntent::Start { press_id: 1 }
        );
        assert_eq!(
            interpreter.release(99),
            HotkeyIntent::Noop,
            "a stale release must not affect the current generation"
        );
        assert_eq!(
            interpreter.press(
                2,
                start + Duration::from_millis(300),
                DictationPhase::Recording,
                false
            ),
            HotkeyIntent::Stop
        );
    }

    #[test]
    fn debounce_suppresses_a_press_release_press_burst() {
        let start = Instant::now();
        let mut interpreter = HotkeyInterpreter::default();

        assert_eq!(
            interpreter.press(1, start, DictationPhase::Idle, false),
            HotkeyIntent::Start { press_id: 1 }
        );
        assert_eq!(interpreter.release(1), HotkeyIntent::Noop);
        assert_eq!(
            interpreter.press(2, start + Duration::from_millis(100), DictationPhase::Idle, false),
            HotkeyIntent::Noop
        );
    }

    #[test]
    fn terminal_cooldown_drops_issue_545_third_press() {
        let start = Instant::now();
        let mut interpreter = HotkeyInterpreter::default();

        assert_eq!(
            interpreter.press(1, start, DictationPhase::Idle, false),
            HotkeyIntent::Start { press_id: 1 }
        );
        assert_eq!(
            interpreter.press(
                2,
                start + Duration::from_millis(300),
                DictationPhase::Recording,
                false
            ),
            HotkeyIntent::Stop
        );
        interpreter.terminal(start + Duration::from_millis(310));
        assert_eq!(
            interpreter.press(3, start + Duration::from_millis(400), DictationPhase::Idle, false),
            HotkeyIntent::Noop,
            "#545: the third press must not restart inside the terminal boundary window"
        );
        assert_eq!(interpreter.release(3), HotkeyIntent::Noop);
        // 冷却窗口刻意压到 150ms（转写/润色阶段已经天然拦住回弹连按）：
        // 窗口一过，用户主动再按必须立即生效。
        assert_eq!(
            interpreter.press(4, start + Duration::from_millis(560), DictationPhase::Idle, false),
            HotkeyIntent::Start { press_id: 4 },
            "the terminal cooldown must not outlast its boundary-race purpose"
        );
    }

    #[test]
    fn cooldown_dropped_press_does_not_consume_the_debounce_window() {
        let start = Instant::now();
        let mut interpreter = HotkeyInterpreter::default();

        assert_eq!(
            interpreter.press(1, start, DictationPhase::Idle, false),
            HotkeyIntent::Start { press_id: 1 }
        );
        assert_eq!(
            interpreter.press(
                2,
                start + Duration::from_millis(1000),
                DictationPhase::Recording,
                false
            ),
            HotkeyIntent::Stop
        );
        // 转写+润色花了一秒，会话在 +2000 收尾；冷却到 +2150。
        interpreter.terminal(start + Duration::from_millis(2000));
        assert_eq!(
            interpreter.press(3, start + Duration::from_millis(2100), DictationPhase::Idle, false),
            HotkeyIntent::Noop,
            "press inside the cooldown is dropped"
        );
        // 冷却刚结束 50ms 用户就重按：若 press3 消耗了防抖窗口，这次会再被吞。
        assert_eq!(
            interpreter.press(4, start + Duration::from_millis(2200), DictationPhase::Idle, false),
            HotkeyIntent::Start { press_id: 4 },
            "a press dropped by the cooldown must not push the next press into the debounce window"
        );
    }

    #[test]
    fn issue_856_processing_press_stays_discarded_after_terminal() {
        let start = Instant::now();
        let mut interpreter = HotkeyInterpreter::default();

        assert_eq!(
            interpreter.press(1, start, DictationPhase::Transcribing, false),
            HotkeyIntent::Noop,
            "#856: a press dequeued during processing is not a future start request"
        );
        interpreter.terminal(start + Duration::from_millis(10));
        assert_eq!(interpreter.release(1), HotkeyIntent::Noop);
        assert_eq!(
            interpreter.press(2, start + Duration::from_millis(100), DictationPhase::Idle, false),
            HotkeyIntent::Noop,
            "a queued follow-up edge inside terminal cooldown must also be dropped"
        );
    }

    #[test]
    fn combined_during_start_cancels_the_same_press_and_clears_debounce() {
        let start = Instant::now();
        let mut interpreter = HotkeyInterpreter::default();

        assert_eq!(
            interpreter.press(1, start, DictationPhase::Idle, false),
            HotkeyIntent::Start { press_id: 1 }
        );
        assert_eq!(
            interpreter.combined(1),
            HotkeyIntent::Cancel { press_id: 1 }
        );
        interpreter.combo_cancelled(1);
        assert_eq!(
            interpreter.press(2, start + Duration::from_millis(1), DictationPhase::Idle, false),
            HotkeyIntent::Start { press_id: 2 },
            "a cancelled keyboard combo must not consume the next debounce window"
        );
    }

    #[test]
    fn combined_during_start_await_is_rechecked_after_setup() {
        let start = Instant::now();
        let mut interpreter = HotkeyInterpreter::default();

        assert_eq!(
            interpreter.press(7, start, DictationPhase::Idle, false),
            HotkeyIntent::Start { press_id: 7 }
        );
        assert_eq!(
            interpreter.combined(7),
            HotkeyIntent::Cancel { press_id: 7 }
        );
        assert!(
            interpreter.start_finished(7, true),
            "the start task must observe a Combined edge that arrived while it awaited setup"
        );
    }

    #[test]
    fn pending_combined_queue_preserves_overtaking_press_ids() {
        let start = Instant::now();
        let mut interpreter = HotkeyInterpreter::default();

        assert_eq!(interpreter.combined(11), HotkeyIntent::Noop);
        assert_eq!(interpreter.combined(12), HotkeyIntent::Noop);
        assert_eq!(
            interpreter.press(11, start, DictationPhase::Idle, true),
            HotkeyIntent::Noop
        );
        assert_eq!(
            interpreter.press(
                12,
                start + Duration::from_millis(1),
                DictationPhase::Idle,
                true,
            ),
            HotkeyIntent::Noop
        );
        assert_eq!(
            interpreter.press(
                13,
                start + Duration::from_millis(2),
                DictationPhase::Idle,
                false,
            ),
            HotkeyIntent::Start { press_id: 13 }
        );
    }
}
