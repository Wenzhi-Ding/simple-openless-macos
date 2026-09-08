#![cfg_attr(target_os = "linux", allow(dead_code, unused_variables))]
//! Tauri command surface — every IPC entry the React UI invokes lives here.
//!
//! issue: 历史上整个 IPC 表（127 个 `#[tauri::command]` + 跨域 helper）挤在单个
//! 4800 行的 `commands.rs` 里。按单一职责拆成 `commands/` 下的域模块（settings /
//! credentials / providers / history / …），每个文件聚焦一个领域。对外路径保持不变：
//! 本模块用 `pub use <domain>::*` glob 重导出每个子模块，`commands::<name>` 仍然解析，
//! `lib.rs` 的 `generate_handler!` 清单与类型引用零改动。`#[tauri::command]` 生成的
//! `__cmd__<name>` 伴生项也随 glob 一并重导出——这是 Tauri 拆分命令文件的标准做法。

use std::sync::Arc;

#[cfg(not(mobile))]
use parking_lot::Mutex;
#[cfg(not(mobile))]
use tauri::Manager;
use tauri::State;

// 跨域共享的 crate 级导入：以 `pub(crate) use` 重导出，子模块用 `use super::*;`
// 即可拿到，避免在 16 个文件里重复同一组 import。
pub(crate) use serde::Serialize;
pub(crate) use serde_json::Value;
pub(crate) use tauri::{AppHandle, Emitter, Window};

#[cfg(not(mobile))]
pub(crate) use crate::asr::local::SherpaOnnxRuntime;
pub(crate) use crate::coordinator::Coordinator;
pub(crate) use crate::permissions::PermissionStatus;
pub(crate) use crate::persistence::sync_style_pack_preferences;
#[cfg(not(mobile))]
pub(crate) use crate::recorder::{AudioConsumer, Recorder};
#[cfg(not(mobile))]
pub(crate) use crate::types::WindowsImeStatus;
pub(crate) use crate::types::{
    builtin_style_pack_id, default_active_style_pack_id, ActivityDay,
    AndroidAccessibilityRecoveryOutcome, AndroidAccessibilityRecoveryResult,
    AndroidAccessibilityStatus, AndroidOverlayStatus, AndroidShizukuStatus,
    ChineseScriptPreference, ComboBinding, CorrectionRule, CredentialsStatus, DictationSession,
    DictionaryEntry, HotkeyCapability, HotkeyStatus, OutputLanguagePreference, PolishMode,
    ShortcutBinding, StylePack, StylePackHotkey, StylePackKind, StylePackRuntimeDiagnostics,
    StyleSystemPrompts, UpdateChannel, UserPreferences, VocabPresetStore,
};

mod dictation;
mod dictionary;
mod history;
mod hotkeys;
#[cfg(not(mobile))]
mod local_asr;
mod local_llm;
mod misc;
mod permissions_cmds;
#[cfg(all(not(mobile), debug_assertions))]
mod selection_polish;
#[cfg(not(mobile))]
mod selection_polish_preview;
#[cfg(all(not(mobile), target_os = "windows"))]
mod selection_voice;
mod settings;
#[cfg(not(mobile))]
mod sherpa_asr;
mod style_packs;

pub use dictation::*;
pub use dictionary::*;
pub use history::*;
pub use hotkeys::*;
#[cfg(not(mobile))]
pub use local_asr::*;
pub use local_llm::*;
pub use misc::*;
pub use permissions_cmds::*;
#[cfg(not(mobile))]
pub use settings::*;
// sherpa_onnx_asr_* 命令整组 `#[cfg(target_os = "windows")]`（见 lib.rs 的
// generate_handler! 清单）。非 Windows 平台这组 glob 重导出无人引用，会触发
// unused_imports；这是平台 cfg 的正常结果，不是真正的死代码。
#[cfg(all(not(mobile), debug_assertions))]
pub use selection_polish::*;
#[cfg(not(mobile))]
pub use selection_polish_preview::*;
#[cfg(all(not(mobile), target_os = "windows"))]
pub use selection_voice::*;
#[cfg(not(mobile))]
#[allow(unused_imports)]
pub use sherpa_asr::*;
pub use style_packs::*;

pub(crate) type CoordinatorState<'a> = State<'a, Arc<Coordinator>>;
pub(crate) type CoreState<'a> = State<'a, Arc<openless_core::OpenLessBackend>>;
#[cfg(not(mobile))]
pub type MicrophoneMonitorState = Mutex<Option<Recorder>>;
#[cfg(not(mobile))]
pub type TrayMicrophoneMenuState = Mutex<Vec<TrayMicrophoneMenuItem>>;

#[cfg(not(mobile))]
pub struct TrayMicrophoneMenuItem {
    pub id: String,
    pub device_name: String,
    pub item: tauri::menu::CheckMenuItem<tauri::Wry>,
}

#[cfg(not(mobile))]
pub fn sync_tray_microphone_selection(items: &[TrayMicrophoneMenuItem], device_name: &str) {
    for item in items {
        let _ = item.item.set_checked(item.device_name == device_name);
    }
}

#[cfg(not(mobile))]
pub(crate) struct LevelProbeConsumer;

#[cfg(not(mobile))]
impl AudioConsumer for LevelProbeConsumer {
    fn consume_pcm_chunk(&self, _pcm: &[u8]) {}
}

// ─────────────────────────── 跨域共享校验 helper ───────────────────────────

/// UUID-v4 字面校验：36 字符 + 5 段 `-` 分隔（8-4-4-4-12）+ 仅 ASCII 十六进制。
/// 用于 install/detail/like —— pack_id 来自远端服务器，必须是它发的 UUID。
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

/// 本地 style pack id 白名单：`[A-Za-z0-9._-]`、长度 1..=128。
/// 上传走本地 id（`builtin.light` / 用户自取 slug / UUID 都可），不是远端 UUID。
/// 仍阻断 `..` / `/` / `\` / 控制字符，避免 path traversal 进临时 zip 文件名。
pub(crate) fn is_valid_local_pack_id(s: &str) -> bool {
    if s.is_empty() || s.len() > 128 {
        return false;
    }
    s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
}

// ─── 在系统文件管理器中打开路径（三个 ASR 模块共用，cfg 分平台）───

#[cfg(target_os = "windows")]
pub(crate) fn open_path_in_file_manager(path: &std::path::Path) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide_null(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let operation = wide_null("open");
    let target = wide_null(&path.display().to_string());
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(operation.as_ptr()),
            PCWSTR(target.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        Err(format!("ShellExecuteW failed: {}", result.0 as isize))
    } else {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn open_path_in_file_manager(path: &std::path::Path) -> Result<(), String> {
    std::process::Command::new("open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
pub(crate) fn open_path_in_file_manager(path: &std::path::Path) -> Result<(), String> {
    std::process::Command::new("xdg-open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{is_valid_local_pack_id, is_valid_session_id};
    use crate::types::{ComboBinding, HotkeyBinding, HotkeyMode, HotkeyTrigger, ShortcutBinding, UserPreferences};

















    #[test]
    fn sync_dictation_hotkey_sets_modifier_trigger_and_clears_combo() {
        let mut prefs = UserPreferences {
            hotkey: HotkeyBinding {
                trigger: HotkeyTrigger::Custom,
                mode: HotkeyMode::Toggle,
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
                mode: HotkeyMode::Toggle,
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
                mode: HotkeyMode::Toggle,
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
    fn validate_combo_hotkey_rejects_bare_shift() {
        let result = super::validate_combo_hotkey(ComboBinding {
            primary: "Shift".into(),
            modifiers: vec![],
        });

        assert!(result.is_err());
    }

    #[test]
    fn combo_hotkey_bare_shift_rejection_matches_dictation_setter() {
        let binding = ShortcutBinding {
            primary: "Shift".into(),
            modifiers: vec![],
        };

        assert_eq!(
            super::reject_bare_shift_dictation_shortcut(&binding),
            Err("Shift 单键目前只能用于翻译快捷键".into())
        );
    }














}