//! Configuration arrival as a composition contract.
//!
//! Literal-construction rules catch a surface that hard-builds `CoderRunConfig`.
//! They cannot catch a setting that parses, defaults, and is never the value the
//! operator wrote. That failure crossed resolution, deserialization, assembly,
//! and runtime use — a silent default can disable a safety feature on every
//! surface.
//!
//! Each row writes one safety-critical `[coder]` field into a real config dir,
//! loads it through [`crate::load_config`], assembles via
//! [`liberado_coder_core::CoderTuning::from_value`] then `run_config()`, and
//! observes the changed value on the run config. Extend the table one field at
//! a time; do not add another config framework.

use super::{TOPOLOGY_FILE, TUNING_FILE, load_config};
use liberado_coder_core::CoderTuning;

/// Field key as written in `tuning.toml`, the file body, and the runtime check.
struct ArrivalCase {
    field: &'static str,
    tuning_toml: &'static str,
    expect: Expect,
}

/// The observation on assembled [`liberado_coder_core::CoderRunConfig`].
enum Expect {
    GateEnabled(bool),
    CoderMaxTurns(u32),
    ProgressReadOnlyTurnLimit(u32),
    HashlineEnabled(bool),
}

const TOPOLOGY_TOML: &str = "vault_path = \"/tmp/paydown5-arrival-vault\"\n";

const CASES: &[ArrivalCase] = &[
    ArrivalCase {
        field: "gate.enabled",
        tuning_toml: "[coder.gate]\nenabled = true\n",
        expect: Expect::GateEnabled(true),
    },
    ArrivalCase {
        field: "coder.max_turns",
        // Serde replaces the whole role table; `model` has no field default.
        // The default model is restated so the non-default value under test is max_turns.
        tuning_toml: "[coder.coder]\nmodel = \"deepseek-v4-pro\"\nmax_turns = 12\n",
        expect: Expect::CoderMaxTurns(12),
    },
    ArrivalCase {
        field: "progress.read_only_turn_limit",
        // `ProgressPolicy` has no serde default, so a partial table will not load.
        tuning_toml: "\
[coder.progress]
read_only_turn_limit = 7
same_tool_limit = 10
validation_repeat_limit = 2
max_attempts = 3
event_preview_max_chars = 500
",
        expect: Expect::ProgressReadOnlyTurnLimit(7),
    },
    ArrivalCase {
        field: "hashline.enabled",
        tuning_toml: "[coder.hashline]\nenabled = true\n",
        expect: Expect::HashlineEnabled(true),
    },
];

fn load_opaque_coder(tuning_toml: &str) -> toml::Value {
    let _guard = crate::survivor_tests::env_lock().lock().unwrap();
    let data = tempfile::TempDir::new().unwrap();
    let _env = crate::survivor_tests::EnvGuard::set("LIBERADO_DATA_DIR", data.path());

    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join(TOPOLOGY_FILE), TOPOLOGY_TOML).unwrap();
    std::fs::write(dir.path().join(TUNING_FILE), tuning_toml).unwrap();

    let (config, provenance) = load_config(Some(dir.path())).expect("valid fixture must load");
    assert_eq!(
        provenance.tuning.as_deref(),
        Some(TUNING_FILE),
        "the written tuning.toml must be the resolved tuning source"
    );
    config
        .tuning
        .coder
        .expect("load_config dropped the opaque [coder] table")
}

fn assemble_run_config(tuning_toml: &str) -> liberado_coder_core::CoderRunConfig {
    let coder = load_opaque_coder(tuning_toml);
    CoderTuning::from_value(Some(&coder))
        .expect("opaque [coder] must assemble")
        .run_config()
}

fn assert_runtime(run: &liberado_coder_core::CoderRunConfig, case: &ArrivalCase) {
    match case.expect {
        Expect::GateEnabled(expected) => {
            assert_eq!(
                run.gate.enabled, expected,
                "{} must arrive on CoderRunConfig (silent default disables the completion gate)",
                case.field
            );
        }
        Expect::CoderMaxTurns(expected) => {
            assert_eq!(
                run.coder.max_turns,
                Some(expected),
                "{} must arrive on CoderRunConfig",
                case.field
            );
        }
        Expect::ProgressReadOnlyTurnLimit(expected) => {
            assert_eq!(
                run.progress.read_only_turn_limit, expected,
                "{} must arrive on CoderRunConfig",
                case.field
            );
        }
        Expect::HashlineEnabled(expected) => {
            assert_eq!(
                run.hashline.enabled, expected,
                "{} must arrive on CoderRunConfig (silent default disables hashline edits)",
                case.field
            );
        }
    }
}

#[test]
fn safety_critical_coder_tuning_fields_arrive_through_real_resolution() {
    for case in CASES {
        let run = assemble_run_config(case.tuning_toml);
        assert_runtime(&run, case);
    }
}
