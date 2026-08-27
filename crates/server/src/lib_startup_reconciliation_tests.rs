//! Split from `lib.rs` for module-health boundaries.

/// F7 is a startup fix, not only a hub helper. Keep the production call after every pack is
/// registered (so `can_resume` is authoritative) and before the hub is exposed to callers.
#[test]
fn run_reconciles_parked_sessions_after_pack_registration() {
    let source = include_str!("lib.rs");
    let run = source
        .split_once("pub async fn run(vault_path: String)")
        .and_then(|(_, tail)| tail.split_once("pub fn config_check("))
        .map(|(body, _)| body)
        .expect("server source must contain the production run body");

    let reconcile = run
        .find("goals_hub.reconcile_parked_at_startup().await")
        .expect("daemon startup must call parked-session reconciliation");
    for registration in [
        "goals_hub.register_pack(Arc::new(liberado_session::LifeOpsDemoRunner))",
        "goals_hub.register_pack(Arc::clone(pack)",
        "goals_hub.register_pack(Arc::new(pack))",
    ] {
        let registered = run
            .find(registration)
            .unwrap_or_else(|| panic!("missing production pack registration: {registration}"));
        assert!(
            registered < reconcile,
            "parked sessions must be classified only after all packs are registered"
        );
    }
    let exposed = run
        .find("let goals = Arc::new(goals_hub);")
        .expect("production hub must be exposed through Arc");
    assert!(
        reconcile < exposed,
        "startup reconciliation must finish before routes or workers can use the hub"
    );
}
