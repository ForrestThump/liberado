//! Split from `assemble.rs`: kills the baseline campaign's survivors.
//!
//! Pins the surface hashline override and each entry path's distinctive
//! policy choices. (The entry constructors enumerate every `ProductionSurface`
//! field explicitly, so a dropped binding no longer compiles — these tests pin
//! the *values* those bindings must carry.)

use super::*;
use liberado_coder_core::CoderGateConfig;

#[test]
fn the_surface_hashline_overrides_tuning() {
    let tuning = CoderTuning {
        hashline: HashlineConfig {
            enabled: true,
            hash_length: 8,
        },
        ..CoderTuning::default()
    };
    let surface = ProductionSurface {
        task: CoderTask::new("t", "d"),
        workspace_path: std::env::temp_dir(),
        hashline: Some(HashlineConfig {
            enabled: true,
            hash_length: 6,
        }),
        ..ProductionSurface::default()
    };
    let assembled = assemble_production_run(&tuning, surface);
    assert_eq!(
        assembled.request.config.hashline,
        HashlineConfig {
            enabled: true,
            hash_length: 6,
        },
        "the surface's hashline wins over tuning"
    );
    assert_eq!(
        assembled.provenance.source_of("hashline"),
        Some("surface.hashline")
    );
}

#[test]
fn pack_surface_pins_its_policies() {
    let args = entry::PackSurfaceArgs {
        task: CoderTask::new("t", "d"),
        workspace_path: PathBuf::from("/ws/pack"),
        sandbox: SandboxSpec::Worktree,
        coder_role: CoderRoleConfig::default(),
        mode: CodingMode::Normal,
        command_policy: CommandPolicy::default(),
        path_policy: PathPolicy::default(),
        hashline: HashlineConfig::default(),
    };
    let s = entry::pack_surface(args);
    assert!(
        matches!(s.sandbox, SandboxSpec::Worktree),
        "pack honours its sandbox arg"
    );
    assert!(matches!(s.critic, CriticPolicy::Disabled));
    assert!(matches!(s.repair, RepairPolicy::MirrorCoder));
    assert!(
        matches!(s.empty_verifiers, EmptyVerifiersPolicy::LeaveEmpty),
        "the contract fills verifiers later"
    );
    assert!(matches!(s.trace_dir, TraceDirPolicy::AsConfigured));
    assert!(s.disable_planner);
    assert!(!s.default_empty_path_policy);
    assert_eq!(s.attempt, 0);
    assert!(s.prior_feedback.is_empty());
}

#[test]
fn acp_surface_pins_its_policies() {
    let s = entry::acp_surface(
        CoderTask::new("t", "d"),
        PathBuf::from("/ws/acp"),
        None,
        None,
        2,
        vec!["prior".into()],
    );
    assert!(matches!(s.critic, CriticPolicy::ReviewerWithLoadedPrompt));
    assert!(matches!(s.repair, RepairPolicy::FromTuning));
    assert!(matches!(
        s.empty_verifiers,
        EmptyVerifiersPolicy::DefaultForWorkspace
    ));
    assert!(matches!(s.trace_dir, TraceDirPolicy::DataDirFallback));
    assert!(s.default_empty_path_policy);
    assert_eq!(s.attempt, 2);
    assert_eq!(s.prior_feedback, vec!["prior".to_string()]);
    assert_eq!(s.workspace.base_ref, "HEAD");
}

#[test]
fn runner_surface_pins_its_policies() {
    let s = entry::runner_surface(
        CoderTask::new("t", "d"),
        PathBuf::from("/ws/runner"),
        Some("m".into()),
        Some(9),
    );
    assert!(matches!(s.critic, CriticPolicy::Disabled));
    assert!(matches!(s.repair, RepairPolicy::None));
    assert!(matches!(
        s.empty_verifiers,
        EmptyVerifiersPolicy::DefaultForWorkspace
    ));
    assert!(matches!(s.trace_dir, TraceDirPolicy::RelativeToWorkspace));
    assert!(!s.default_empty_path_policy);
    assert_eq!(s.model_override.as_deref(), Some("m"));
    assert_eq!(s.max_turns, Some(9));
}
