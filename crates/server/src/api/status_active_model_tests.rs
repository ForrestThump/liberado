//! Split from `status.rs` for module-health boundaries.

use super::*;
use crate::state::AppState;

/// Live provider model wins over the boot snapshot; neither present means none — a stub
/// here would mislabel every model list and selection.
#[tokio::test]
async fn active_model_prefers_live_provider_then_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::for_test(
        Arc::new(liberado_session_store::SessionStore::open(dir.path()).await),
        None,
        std::env::temp_dir(),
    );
    assert_eq!(active_model(&state), None);

    let mut with_snapshot = AppState::for_test(state.sessions.clone(), None, std::env::temp_dir());
    with_snapshot.model_name = Some("boot-model".into());
    assert_eq!(active_model(&with_snapshot).as_deref(), Some("boot-model"));

    with_snapshot.provider = Some(Arc::new(liberado_provider::MockProvider::new("live-model")));
    assert_eq!(
        active_model(&with_snapshot).as_deref(),
        Some("live-model"),
        "the live hot-swapped model wins over the boot-time snapshot"
    );
}
