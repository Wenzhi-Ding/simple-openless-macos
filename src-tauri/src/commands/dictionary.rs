use super::*;

#[tauri::command]
pub fn list_vocab(core: CoreState<'_>) -> Result<Vec<openless_core::DictionaryEntry>, String> {
    core.list_vocabulary().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn add_vocab(
    core: CoreState<'_>,
    phrase: String,
    note: Option<String>,
) -> Result<openless_core::DictionaryEntry, String> {
    core.add_vocabulary(phrase, note)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn remove_vocab(core: CoreState<'_>, id: String) -> Result<(), String> {
    core.remove_vocabulary(&id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn set_vocab_enabled(core: CoreState<'_>, id: String, enabled: bool) -> Result<(), String> {
    core.set_vocabulary_enabled(&id, enabled)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn copy_text_to_clipboard(text: String) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    crate::insertion::copy_text_to_clipboard(&text)
}

#[tauri::command]
pub fn dismiss_insert_fallback_card(coord: CoordinatorState<'_>) {
    coord.dismiss_insert_fallback_card();
}

#[tauri::command]
pub fn report_insert_fallback_card_height(
    coord: CoordinatorState<'_>,
    presentation_id: u64,
    height: f64,
) -> Result<(), String> {
    coord.report_insert_fallback_card_height(presentation_id, height)
}
