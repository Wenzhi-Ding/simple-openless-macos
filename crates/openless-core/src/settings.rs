use serde::{Deserialize, Serialize};

use crate::errors::BackendError;
use crate::shared_types::{HotkeyMode, ShortcutBinding, UserPreferences};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsCollisionPolicy {
    Reject,
    Reconcile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsUpdateOptions {
    pub collision_policy: SettingsCollisionPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_preferences_revision: Option<u64>,
}

impl SettingsUpdateOptions {
    pub const STRICT: Self = Self {
        collision_policy: SettingsCollisionPolicy::Reject,
        expected_preferences_revision: None,
    };

    pub const SETTINGS_DOCUMENT: Self = Self {
        collision_policy: SettingsCollisionPolicy::Reconcile,
        expected_preferences_revision: None,
    };

    pub const fn at_revision(mut self, revision: u64) -> Self {
        self.expected_preferences_revision = Some(revision);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyRuntimeTarget {
    pub dictation: ShortcutBinding,
    pub dictation_mode: HotkeyMode,
    pub open_app: Option<ShortcutBinding>,
}

impl From<&UserPreferences> for HotkeyRuntimeTarget {
    fn from(preferences: &UserPreferences) -> Self {
        Self {
            dictation: preferences.dictation_hotkey.clone(),
            dictation_mode: preferences.hotkey.mode,
            open_app: preferences.open_app_hotkey.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsValueChange<T> {
    pub previous: T,
    pub next: T,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsEffectPlan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotkeys: Option<SettingsValueChange<HotkeyRuntimeTarget>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_asr_provider: Option<SettingsValueChange<String>>,
}

impl SettingsEffectPlan {
    pub fn between(previous: &UserPreferences, next: &UserPreferences) -> Self {
        fn changed<T: PartialEq>(previous: T, next: T) -> Option<SettingsValueChange<T>> {
            (previous != next).then_some(SettingsValueChange { previous, next })
        }

        Self {
            hotkeys: changed(previous.into(), next.into()),
            active_asr_provider: changed(
                previous.active_asr_provider.clone(),
                next.active_asr_provider.clone(),
            ),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.hotkeys.is_none() && self.active_asr_provider.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsEffectKind {
    ActiveAsrProvider,
    Hotkeys,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsEffectReceipt {
    pub applied: Vec<SettingsEffectKind>,
}

#[derive(Debug, Clone)]
pub struct SettingsEffectFailure {
    pub error: BackendError,
    pub receipt: SettingsEffectReceipt,
}

impl SettingsEffectFailure {
    pub fn before_side_effect(error: BackendError) -> Self {
        Self {
            error,
            receipt: SettingsEffectReceipt::default(),
        }
    }

    pub fn after_side_effect(error: BackendError, receipt: SettingsEffectReceipt) -> Self {
        Self { error, receipt }
    }
}

/// Platform adapter for the settings transaction.
///
/// `prepare` and `commit` both run before preferences are persisted. Adapters
/// must consume only the explicit targets in `SettingsEffectPlan`; they must not
/// read a staged settings document. `restore` must be idempotent and restore only
/// the effects named by the receipt.
pub trait SettingsRuntime: Send + Sync {
    fn prepare(
        &self,
        _plan: &SettingsEffectPlan,
    ) -> Result<SettingsEffectReceipt, SettingsEffectFailure> {
        Ok(SettingsEffectReceipt::default())
    }

    fn commit(
        &self,
        _plan: &SettingsEffectPlan,
        _receipt: &mut SettingsEffectReceipt,
    ) -> Result<(), SettingsEffectFailure> {
        Ok(())
    }

    fn restore(
        &self,
        _plan: &SettingsEffectPlan,
        _receipt: &SettingsEffectReceipt,
    ) -> Result<(), BackendError> {
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct NoopSettingsRuntime;

impl SettingsRuntime for NoopSettingsRuntime {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsUpdateOutcome {
    pub preferences: UserPreferences,
    pub reconciled_hotkey_count: usize,
    pub effects: SettingsEffectPlan,
}
