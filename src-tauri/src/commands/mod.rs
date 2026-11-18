//! Tauri command surface — every IPC entry the React UI invokes lives here.
//!
//! issue: 历史上整个 IPC 表（127 个 `#[tauri::command]` + 跨域 helper）挤在单个
//! 4800 行的 `commands.rs` 里。按单一职责拆成 `commands/` 下的域模块（settings /
//! credentials / providers / history / …），每个文件聚焦一个领域。对外路径保持不变：
//! 本模块用 `pub use <domain>::*` glob 重导出每个子模块，`commands::<name>` 仍然解析，
//! `lib.rs` 的 `generate_handler!` 清单与类型引用零改动。`#[tauri::command]` 生成的
//! `__cmd__<name>` 伴生项也随 glob 一并重导出——这是 Tauri 拆分命令文件的标准做法。
pub(crate) use crate::coordinator::Coordinator;
pub(crate) use crate::permissions::PermissionStatus;
pub(crate) use crate::recorder::{AudioConsumer, Recorder};
pub(crate) use crate::types::{
    ActivityDay, DictationSession, HotkeyCapability, HotkeyStatus, ShortcutBinding,
    UserPreferences,
};
use parking_lot::Mutex;
pub(crate) use serde::Serialize;
use std::sync::Arc;
use tauri::Manager;
use tauri::State;
pub(crate) use tauri::{AppHandle, Emitter};
mod cue;
mod dictation;
mod dictionary;
mod history;
mod hotkeys;
mod local_asr;
mod local_llm;
mod local_ocr;
mod misc;
mod model_download;
mod permissions_cmds;
mod settings;
pub use cue::*;
pub use dictation::*;
pub use dictionary::*;
pub use history::*;
pub use hotkeys::*;
pub use local_asr::*;
pub use local_llm::*;
pub use local_ocr::*;
pub use misc::*;
pub use model_download::*;
pub use permissions_cmds::*;
pub use settings::*;
pub(crate) type CoordinatorState<'a> = State<'a, Arc<Coordinator>>;
pub(crate) type CoreState<'a> = State<'a, Arc<openless_core::OpenLessBackend>>;
pub type MicrophoneMonitorState = Mutex<Option<Recorder>>;
pub type TrayMicrophoneMenuState = Mutex<Vec<TrayMicrophoneMenuItem>>;
pub struct TrayMicrophoneMenuItem {
    pub id: String,
    pub device_name: String,
    pub item: tauri::menu::CheckMenuItem<tauri::Wry>,
}
pub fn sync_tray_microphone_selection(items: &[TrayMicrophoneMenuItem], device_name: &str) {
    for item in items {
        let _ = item.item.set_checked(item.device_name == device_name);
    }
}
pub(crate) struct LevelProbeConsumer;
impl AudioConsumer for LevelProbeConsumer {
    fn consume_pcm_chunk(&self, _pcm: &[u8]) {}
}
/// UUID-v4 字面校验：36 字符 + 5 段 `-` 分隔（8-4-4-4-12）+ 仅 ASCII 十六进制。
/// 用于录音读取/导出/重新转写命令的会话号入参校验（会话号由前端透传）。
pub(crate) fn is_valid_session_id(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    let bytes = s.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        let is_dash_position = matches!(i, 8 | 13 | 18 | 23);
        if is_dash_position {
            if *b != b'-' {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}
#[cfg(test)]
mod tests {
    use crate::types::{
        ComboBinding, HotkeyBinding, HotkeyTrigger, ShortcutBinding, UserPreferences,
    };
    #[test]
    fn sync_dictation_hotkey_sets_modifier_trigger_and_clears_combo() {
        let mut prefs = UserPreferences {
            hotkey: HotkeyBinding {
                trigger: HotkeyTrigger::Custom,
                keys: None,
            },
            custom_combo_hotkey: Some(ComboBinding {
                primary: "D".into(),
                modifiers: vec!["cmd".into(), "shift".into()],
            }),
            dictation_hotkey: ShortcutBinding {
                primary: "RightControl".into(),
                modifiers: vec![],
            },
            ..Default::default()
        };
        super::sync_dictation_hotkey_legacy_fields(&mut prefs);
        assert_eq!(prefs.hotkey.trigger, HotkeyTrigger::RightControl);
        assert!(prefs.custom_combo_hotkey.is_none());
    }
    #[test]
    fn sync_dictation_hotkey_sets_custom_trigger_and_combo_binding() {
        let mut prefs = UserPreferences {
            hotkey: HotkeyBinding {
                trigger: HotkeyTrigger::RightControl,
                keys: None,
            },
            dictation_hotkey: ShortcutBinding {
                primary: "D".into(),
                modifiers: vec!["cmd".into(), "shift".into()],
            },
            ..Default::default()
        };
        super::sync_dictation_hotkey_legacy_fields(&mut prefs);
        assert_eq!(prefs.hotkey.trigger, HotkeyTrigger::Custom);
        let combo = prefs.custom_combo_hotkey.expect("combo binding saved");
        assert_eq!(combo.primary, "D");
        assert_eq!(
            combo.modifiers,
            vec!["cmd".to_string(), "shift".to_string()]
        );
    }
    #[test]
    fn sync_dictation_hotkey_clears_empty_custom_binding() {
        let mut prefs = UserPreferences {
            hotkey: HotkeyBinding {
                trigger: HotkeyTrigger::RightControl,
                keys: None,
            },
            custom_combo_hotkey: Some(ComboBinding {
                primary: "D".into(),
                modifiers: vec!["cmd".into(), "shift".into()],
            }),
            dictation_hotkey: ShortcutBinding {
                primary: " ".into(),
                modifiers: vec!["cmd".into()],
            },
            ..Default::default()
        };
        super::sync_dictation_hotkey_legacy_fields(&mut prefs);
        assert_eq!(prefs.hotkey.trigger, HotkeyTrigger::Custom);
        assert!(prefs.custom_combo_hotkey.is_none());
    }
    #[test]
    fn combo_hotkey_bare_shift_rejection_matches_dictation_setter() {
        let binding = ShortcutBinding {
            primary: "Shift".into(),
            modifiers: vec![],
        };
        assert_eq!(
            super::reject_bare_shift_dictation_shortcut(&binding),
            Err("Shift 单键不能作为快捷键，请使用组合键".into())
        );
    }
}
