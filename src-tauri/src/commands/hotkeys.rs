use super::*;
#[cfg(test)]
use openless_core::reject_non_dictation_side_specific_shortcuts;

pub(crate) use openless_core::{
    reject_bare_shift_dictation_shortcut, reject_hotkey_collisions,
    reject_modifier_only_action_shortcut, sync_dictation_hotkey_legacy_fields,
};

#[tauri::command]
pub fn validate_shortcut_binding(binding: ShortcutBinding) -> Result<(), String> {
    crate::shortcut_binding::validate_binding(&binding).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_dictation_hotkey(
    coord: CoordinatorState<'_>,
    binding: ShortcutBinding,
) -> Result<(), String> {
    crate::shortcut_binding::validate_binding(&binding).map_err(|e| e.to_string())?;
    reject_bare_shift_dictation_shortcut(&binding)?;
    let mut prefs = coord.backend().get_preferences();
    prefs.dictation_hotkey = binding;
    sync_dictation_hotkey_legacy_fields(&mut prefs);
    reject_hotkey_collisions(&prefs)?;
    super::settings::persist_strict_settings(&coord, prefs)
}

/// 设置「唤起 App」全局快捷键。`binding == None`（前端传 null）= 停用（同上）。
#[tauri::command]
pub fn set_open_app_hotkey(
    coord: CoordinatorState<'_>,
    binding: Option<ShortcutBinding>,
) -> Result<(), String> {
    if let Some(binding) = binding.as_ref() {
        crate::shortcut_binding::validate_binding(binding).map_err(|e| e.to_string())?;
        crate::shortcut_binding::reject_side_specific_non_dictation(binding)?;
        reject_modifier_only_action_shortcut(binding)?;
    }
    let mut prefs = coord.backend().get_preferences();
    prefs.open_app_hotkey = binding;
    reject_hotkey_collisions(&prefs)?;
    super::settings::persist_strict_settings(&coord, prefs)
}

#[tauri::command]
pub fn validate_combo_hotkey(binding: ComboBinding) -> Result<(), String> {
    let shortcut = ShortcutBinding {
        primary: binding.primary,
        modifiers: binding.modifiers,
    };
    reject_bare_shift_dictation_shortcut(&shortcut)?;
    crate::combo_hotkey::validate_binding(&shortcut).map_err(|e| e.to_string())
}

/// 设置自定义录音组合键并热更新 monitor。
#[tauri::command]
pub fn set_combo_hotkey(coord: CoordinatorState<'_>, binding: ComboBinding) -> Result<(), String> {
    let mut prefs = coord.backend().get_preferences();
    let shortcut = ShortcutBinding {
        primary: binding.primary.clone(),
        modifiers: binding.modifiers.clone(),
    };
    reject_bare_shift_dictation_shortcut(&shortcut)?;
    crate::combo_hotkey::validate_binding(&shortcut).map_err(|e| e.to_string())?;
    prefs.custom_combo_hotkey = Some(binding);
    prefs.dictation_hotkey = shortcut;
    sync_dictation_hotkey_legacy_fields(&mut prefs);
    reject_hotkey_collisions(&prefs)?;
    super::settings::persist_strict_settings(&coord, prefs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_side_specific_dictation_hotkey_on_save() {
        let prefs = UserPreferences {
            dictation_hotkey: ShortcutBinding {
                primary: "D".into(),
                modifiers: vec!["cmd-left".into()],
            },
            ..Default::default()
        };
        assert!(reject_non_dictation_side_specific_shortcuts(&prefs).is_ok());
    }
}
