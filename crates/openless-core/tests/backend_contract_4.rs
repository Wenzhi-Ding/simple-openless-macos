use openless_core::{HistorySource, BACKEND_CONTRACT_VERSION};

#[test]
fn backend_contract_4_fixture_matches_version_and_selection_history_compatibility() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../contract/backend-4.0.json")).unwrap();
    assert_eq!(fixture["contractVersion"], BACKEND_CONTRACT_VERSION);
    let kinds = fixture["backendEvent"]["kinds"].as_array().unwrap();
    assert!(!kinds.iter().any(|kind| kind == "selection_state_changed"));
    assert!(!kinds
        .iter()
        .any(|kind| kind == "vocabulary_suggestions_changed"));
    assert!(fixture["enums"].get("selectionPhase").is_none());
    let source: HistorySource = serde_json::from_str("\"selection_voice_edit\"").unwrap();
    assert_eq!(source, HistorySource::SelectionPolish);
}
