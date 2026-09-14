//! Pure shortcut compatibility helpers shared by host adapters.

use std::collections::BTreeSet;

use crate::shared_types::{ComboBinding, HotkeyTrigger, ShortcutBinding, UserPreferences};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ShortcutBindingError {
    #[error("不支持的修饰键: {0}")]
    UnsupportedModifier(String),
    #[error("不支持的主键: {0}")]
    UnsupportedKey(String),
}

const SIDE_MODIFIER_TAGS: &[&str] = &[
    "cmd-left",
    "cmd-right",
    "ctrl-left",
    "ctrl-right",
    "alt-left",
    "alt-right",
    "shift-left",
    "shift-right",
    "super-left",
    "super-right",
];

pub fn normalize_side_modifier_tag(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "super-left" => "cmd-left".into(),
        "super-right" => "cmd-right".into(),
        tag => tag.to_string(),
    }
}

pub fn is_side_specific_modifier_tag(raw: &str) -> bool {
    SIDE_MODIFIER_TAGS.contains(&normalize_side_modifier_tag(raw).as_str())
}

pub fn binding_requires_side_aware_hook(binding: &ShortcutBinding) -> bool {
    !binding.modifiers.is_empty()
        && binding
            .modifiers
            .iter()
            .any(|tag| is_side_specific_modifier_tag(tag))
}

pub const SIDE_SPECIFIC_NON_DICTATION_MSG: &str =
    "Side-specific modifier shortcuts are only supported for dictation start/stop.";

pub fn reject_side_specific_non_dictation(binding: &ShortcutBinding) -> Result<(), String> {
    if binding_requires_side_aware_hook(binding) {
        return Err(SIDE_SPECIFIC_NON_DICTATION_MSG.to_string());
    }
    Ok(())
}

fn normalize_modifier_tag(raw: &str) -> String {
    let tag = raw.trim().to_ascii_lowercase();
    if is_side_specific_modifier_tag(&tag) {
        return tag;
    }
    tag
}

fn physical_class_from_generic_tag(tag: &str) -> String {
    match tag {
        "ctrl" | "control" => "Control".to_string(),
        "alt" | "option" | "opt" => "Alt".to_string(),
        "shift" => "Shift".to_string(),
        "cmd" | "command" | "super" | "meta" | "win" => "Super".to_string(),
        other => other.to_string(),
    }
}

fn physical_modifier_class(raw: &str) -> String {
    let tag = normalize_side_modifier_tag(raw);
    if is_side_specific_modifier_tag(&tag) {
        if tag.starts_with("cmd-") || tag.starts_with("super-") {
            return "Super".to_string();
        }
        if tag.starts_with("ctrl-") {
            return "Control".to_string();
        }
        if tag.starts_with("alt-") {
            return "Alt".to_string();
        }
        if tag.starts_with("shift-") {
            return "Shift".to_string();
        }
    }
    physical_class_from_generic_tag(&normalize_modifier_tag(raw))
}

fn physical_modifier_set(binding: &ShortcutBinding) -> BTreeSet<String> {
    binding
        .modifiers
        .iter()
        .map(|raw| physical_modifier_class(raw))
        .collect()
}

fn normalize_primary(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|character| !matches!(character, ' ' | '-' | '_'))
        .collect::<String>()
        .to_ascii_lowercase()
}

pub fn legacy_modifier_trigger(binding: &ShortcutBinding) -> Option<HotkeyTrigger> {
    if !binding.modifiers.is_empty() {
        return None;
    }
    match normalize_primary(&binding.primary).as_str() {
        "rightoption" | "rightalt" => Some(HotkeyTrigger::RightOption),
        "leftoption" | "leftalt" => Some(HotkeyTrigger::LeftOption),
        "rightcontrol" | "rightctrl" => Some(HotkeyTrigger::RightControl),
        "leftcontrol" | "leftctrl" => Some(HotkeyTrigger::LeftControl),
        "rightcommand" | "rightcmd" | "rightsuper" | "rightmeta" => {
            Some(HotkeyTrigger::RightCommand)
        }
        "leftcommand" | "leftcmd" | "leftsuper" | "leftmeta" => Some(HotkeyTrigger::LeftCommand),
        "leftshift" | "shiftleft" => Some(HotkeyTrigger::LeftShift),
        "rightshift" | "shiftright" => Some(HotkeyTrigger::RightShift),
        "fn" | "function" => Some(HotkeyTrigger::Fn),
        "mediaplaypause" | "mediaplay" | "playpause" => Some(HotkeyTrigger::MediaPlayPause),
        _ => None,
    }
}

pub fn bindings_overlap(left: &ShortcutBinding, right: &ShortcutBinding) -> bool {
    let left_legacy = legacy_modifier_trigger(left);
    let right_legacy = legacy_modifier_trigger(right);
    match (left_legacy, right_legacy) {
        (Some(left), Some(right)) => left == right,
        (Some(_), None) | (None, Some(_)) => false,
        (None, None) => {
            if normalize_primary(&left.primary) != normalize_primary(&right.primary) {
                return false;
            }
            let left_side = binding_requires_side_aware_hook(left);
            let right_side = binding_requires_side_aware_hook(right);
            if left_side && right_side {
                let left_modifiers: BTreeSet<String> = left
                    .modifiers
                    .iter()
                    .map(|raw| normalize_side_modifier_tag(raw))
                    .collect();
                let right_modifiers: BTreeSet<String> = right
                    .modifiers
                    .iter()
                    .map(|raw| normalize_side_modifier_tag(raw))
                    .collect();
                return left_modifiers == right_modifiers;
            }
            physical_modifier_set(left) == physical_modifier_set(right)
        }
    }
}

pub fn binding_from_legacy_trigger(trigger: HotkeyTrigger) -> ShortcutBinding {
    let primary = match trigger {
        HotkeyTrigger::RightOption | HotkeyTrigger::RightAlt => "RightOption",
        HotkeyTrigger::LeftOption => "LeftOption",
        HotkeyTrigger::RightControl => "RightControl",
        HotkeyTrigger::LeftControl => "LeftControl",
        HotkeyTrigger::RightCommand => "RightCommand",
        HotkeyTrigger::LeftCommand => "LeftCommand",
        HotkeyTrigger::LeftShift => "LeftShift",
        HotkeyTrigger::RightShift => "RightShift",
        HotkeyTrigger::Fn => "Fn",
        HotkeyTrigger::MediaPlayPause => "MediaPlayPause",
        HotkeyTrigger::Custom => "RightOption",
    };
    ShortcutBinding {
        primary: primary.into(),
        modifiers: Vec::new(),
    }
}

pub fn validate_shortcut_binding(binding: &ShortcutBinding) -> Result<(), ShortcutBindingError> {
    if legacy_modifier_trigger(binding).is_some() {
        return Ok(());
    }
    if binding.modifiers.is_empty() && binding.primary.eq_ignore_ascii_case("shift") {
        return Ok(());
    }

    validate_primary(&binding.primary)?;
    for raw in &binding.modifiers {
        if binding_requires_side_aware_hook(binding) {
            if !is_side_specific_modifier_tag(raw) {
                return Err(ShortcutBindingError::UnsupportedModifier(raw.clone()));
            }
            continue;
        }
        let normalized = normalize_modifier_tag(raw);
        if !matches!(
            normalized.as_str(),
            "cmd"
                | "command"
                | "super"
                | "meta"
                | "win"
                | "ctrl"
                | "control"
                | "alt"
                | "option"
                | "opt"
                | "shift"
        ) {
            return Err(ShortcutBindingError::UnsupportedModifier(normalized));
        }
    }
    Ok(())
}

fn validate_primary(raw: &str) -> Result<(), ShortcutBindingError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ShortcutBindingError::UnsupportedKey("(空)".into()));
    }
    if trimmed.chars().count() == 1
        && trimmed
            .chars()
            .next()
            .is_some_and(is_supported_shortcut_character)
    {
        return Ok(());
    }
    let upper = trimmed.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "ENTER"
            | "RETURN"
            | "TAB"
            | "ESC"
            | "ESCAPE"
            | "SPACE"
            | "BACKSPACE"
            | "DELETE"
            | "DEL"
            | "HOME"
            | "END"
            | "PAGEUP"
            | "PAGEDOWN"
            | "ARROWUP"
            | "UP"
            | "ARROWDOWN"
            | "DOWN"
            | "ARROWLEFT"
            | "LEFT"
            | "ARROWRIGHT"
            | "RIGHT"
            | "F1"
            | "F2"
            | "F3"
            | "F4"
            | "F5"
            | "F6"
            | "F7"
            | "F8"
            | "F9"
            | "F10"
            | "F11"
            | "F12"
    ) {
        return Ok(());
    }
    Err(ShortcutBindingError::UnsupportedKey(trimmed.to_string()))
}

fn is_supported_shortcut_character(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(
            character,
            ';' | ':'
                | ','
                | '<'
                | '.'
                | '>'
                | '/'
                | '?'
                | '\\'
                | '|'
                | '['
                | '{'
                | ']'
                | '}'
                | '\''
                | '"'
                | '`'
                | '~'
                | '-'
                | '_'
                | '='
                | '+'
                | ' '
                | '!'
                | '@'
                | '#'
                | '$'
                | '%'
                | '^'
                | '&'
                | '*'
                | '('
                | ')'
        )
}

pub fn reject_bare_shift_dictation_shortcut(binding: &ShortcutBinding) -> Result<(), String> {
    if binding.modifiers.is_empty() && binding.primary.eq_ignore_ascii_case("shift") {
        return Err("Shift 单键不能作为快捷键，请使用组合键".into());
    }
    Ok(())
}

pub fn sync_dictation_hotkey_legacy_fields(preferences: &mut UserPreferences) {
    if let Some(trigger) = legacy_modifier_trigger(&preferences.dictation_hotkey) {
        preferences.hotkey.trigger = trigger;
        preferences.custom_combo_hotkey = None;
        return;
    }
    preferences.hotkey.trigger = HotkeyTrigger::Custom;
    preferences.custom_combo_hotkey = if preferences.dictation_hotkey.primary.trim().is_empty() {
        None
    } else {
        Some(ComboBinding {
            primary: preferences.dictation_hotkey.primary.clone(),
            modifiers: preferences.dictation_hotkey.modifiers.clone(),
        })
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn combo(primary: &str, modifiers: &[&str]) -> ShortcutBinding {
        ShortcutBinding {
            primary: primary.to_string(),
            modifiers: modifiers.iter().map(|value| (*value).to_string()).collect(),
        }
    }

    #[test]
    fn validates_shared_shortcut_grammar_without_a_native_hotkey_crate() {
        assert!(validate_shortcut_binding(&combo("D", &["cmd", "shift"])).is_ok());
        assert!(validate_shortcut_binding(&combo("?", &["shift"])).is_ok());
        assert!(validate_shortcut_binding(&combo("F12", &[])).is_ok());
        assert_eq!(
            validate_shortcut_binding(&combo("D", &["hyper"])),
            Err(ShortcutBindingError::UnsupportedModifier("hyper".into()))
        );
        assert_eq!(
            validate_shortcut_binding(&combo("VolumeUp", &[])),
            Err(ShortcutBindingError::UnsupportedKey("VolumeUp".into()))
        );
    }

    #[test]
    fn side_specific_rules_are_shared_by_all_hosts() {
        let side_specific = combo("D", &["cmd-left", "shift-right"]);
        assert!(validate_shortcut_binding(&side_specific).is_ok());
        assert!(binding_requires_side_aware_hook(&side_specific));
        assert_eq!(
            reject_side_specific_non_dictation(&side_specific).unwrap_err(),
            SIDE_SPECIFIC_NON_DICTATION_MSG
        );
        assert!(validate_shortcut_binding(&combo("D", &["cmd-left", "shift"])).is_err());
    }

    #[test]
    fn overlap_and_legacy_conversion_have_one_shared_implementation() {
        assert!(bindings_overlap(
            &combo("D", &["ctrl-left"]),
            &combo("D", &["ctrl"])
        ));
        assert!(!bindings_overlap(
            &combo("D", &["ctrl-left"]),
            &combo("D", &["ctrl", "shift"])
        ));
        let binding = binding_from_legacy_trigger(HotkeyTrigger::RightControl);
        assert_eq!(
            legacy_modifier_trigger(&binding),
            Some(HotkeyTrigger::RightControl)
        );
    }
}
