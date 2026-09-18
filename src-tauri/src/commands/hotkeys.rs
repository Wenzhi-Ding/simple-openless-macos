use super::*;

pub(crate) use openless_core::{reject_bare_shift_dictation_shortcut, sync_dictation_hotkey_legacy_fields};

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
    super::settings::persist_strict_settings(&coord, prefs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_side_specific_dictation_hotkey_on_save() {
        let binding = ShortcutBinding {
            primary: "D".into(),
            modifiers: vec!["cmd-left".into()],
        };
        // 保存路径的实际校验：语法合法 + 非 Shift 单键。side-specific 修饰键
        // 只允许用在听写热键上，这里正是听写热键，应放行。
        assert!(crate::shortcut_binding::validate_binding(&binding).is_ok());
        assert!(reject_bare_shift_dictation_shortcut(&binding).is_ok());
    }
}
