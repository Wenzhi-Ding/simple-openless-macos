//! Local dictation contract tests.
//!
//! 减法改造(纯本地):原文件里依赖 canonical `contract/backend-2.0.json`
//! (跨宿主契约快照,含 RemoteAuthResult / LessComputer 等已移除云远程类型)
//! 与 Less Computer 语音反馈线格式的测试一并移除;保留并迁移仍然适用的
//! 本地听写回归:流式润色插入契约 + 契约版本拒绝。

use openless_core::{require_backend_contract_version, BACKEND_CONTRACT_VERSION};

#[test]
fn runtime_wire_rejects_non_2_contracts() {
    require_backend_contract_version(BACKEND_CONTRACT_VERSION).expect("2.0 contract accepted");
    for bogus in ["1.0.0", "2.0.1", "3.0.0", ""] {
        assert!(require_backend_contract_version(bogus).is_err());
    }
}

#[tokio::test]
async fn core_streams_polish_deltas_through_one_insertion_session() {
    use std::sync::Arc;

    use openless_core::shared_types::WindowsInsertionMode;
    use openless_core::testing::{
        FixtureDictationEngine, FixtureInsertionAction, FixtureTextInserter,
    };
    use openless_core::{
        BackendConfig, BackendDependencies, InMemoryCredentialStore, InsertOutcome,
        NoopSettingsRuntime, PolishDelta, SettingsUpdateOptions, TokioTaskSpawner,
    };

    let data_dir = std::env::temp_dir().join(format!(
        "openless-contract-streaming-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let inserter = FixtureTextInserter::with_outcome(InsertOutcome::Inserted);
    let backend = openless_core::OpenLessBackend::new(
        BackendConfig {
            data_dir: data_dir.clone(),
            ..BackendConfig::default()
        },
        BackendDependencies {
            text_inserter: Arc::new(inserter.clone()),
            dictation_engine: Arc::new(
                FixtureDictationEngine::successful("你好", "你好").with_polish_deltas(vec![
                    PolishDelta {
                        text: "你".to_string(),
                        offset: 0,
                        is_final: false,
                    },
                    PolishDelta {
                        text: "好".to_string(),
                        offset: 1,
                        is_final: false,
                    },
                ]),
            ),
            task_spawner: Arc::new(TokioTaskSpawner),
            credential_store: Arc::new(InMemoryCredentialStore::default()),
            ..BackendDependencies::unsupported()
        },
    )
    .unwrap();
    let mut preferences = backend.get_preferences();
    preferences.windows_insertion_mode = WindowsInsertionMode::SendInput;
    backend
        .update_settings(
            preferences,
            SettingsUpdateOptions::STRICT,
            &NoopSettingsRuntime,
        )
        .unwrap();
    backend.start().await.unwrap();
    backend.start_dictation().await.unwrap();
    backend.stop_dictation().await.unwrap();

    assert!(inserter.actions().iter().any(|action| matches!(
        action,
        FixtureInsertionAction::Write { text, .. } if text == "你好"
    )));
    assert!(inserter.actions().iter().any(|action| matches!(
        action,
        FixtureInsertionAction::Insert { text, .. } if text.is_empty()
    )));
    let _ = std::fs::remove_dir_all(data_dir);
}
