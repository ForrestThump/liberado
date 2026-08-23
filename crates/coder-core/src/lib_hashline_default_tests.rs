//! Split from `lib.rs` for module-health boundaries.

use super::*;
use std::path::Path;

/// Hashline is **off** by default, and that is a measured position rather than a taste.
///
/// #105 turned it on to end a divergence between the two coding paths. A four-run series on
/// one task then measured it: with hashline on, `read_file` returns a line-numbered view
/// while `edit_file` matches raw text, and the model pasted one into the other in 14 of 41
/// calls — the worst anchor failure rate of the four (72%). The same task with hashline off
/// scored best (42%) and produced 159 insertions with no deletions.
///
/// The catalog is exclusive now, so hashline is no longer *broken*; it is simply not the
/// default until a run measures it winning. Flipping this without that measurement should
/// fail here.
#[test]
fn hashline_is_off_until_a_run_measures_it_winning() {
    let config = HashlineConfig::default();
    assert!(
        !config.enabled,
        "turning hashline on is a measured decision; the last measurement said off"
    );
    assert!(
        config.validate().is_ok(),
        "whatever the default is, it must satisfy its own validator: {:?}",
        config.validate()
    );
}

/// The warm-up is on by default, and the timeout must be longer than an honest cold build.
///
/// A ceiling shorter than a real build turns "slow" into "the tree looks broken", which is
/// the failure this replaced: a 120-second command timeout axed every workspace-wide cargo
/// invocation and returned no output at all.
#[test]
fn the_warmup_is_on_and_its_ceiling_is_generous() {
    let config = WorkspaceBuildConfig::default();
    assert!(
        config.warmup,
        "a run should not discover a broken baseline from the model"
    );
    assert!(
        config.warmup_timeout_secs >= 600,
        "a cold build of this workspace is minutes; {}s would report a slow machine as a              broken tree",
        config.warmup_timeout_secs
    );
}

/// The command ceiling must clear a workspace build too, or the model's own checks die the
/// way the warm-up used to.
#[test]
fn the_command_timeout_clears_a_workspace_build() {
    assert!(
        CommandPolicy::default().timeout_secs >= 600,
        "120s returned nothing from every cargo command a run tried"
    );
}

/// No shared cache by default. Cargo locks a target directory, so two concurrent runs queue
/// rather than corrupt — measured — and a run queued behind a cold build times out having
/// done nothing. Sharing is opt-in for the one-run-at-a-time case it is safe for.
#[test]
fn the_shared_cache_is_opt_in() {
    assert!(WorkspaceBuildConfig::default().shared_target_dir.is_none());
}

/// The number in `config.example/tuning.toml` must be the number the code uses.
///
/// This replaced a test comparing `EditConfig::DEFAULT_FUZZY_THRESHOLD` against
/// `EditConfig::default().fuzzy_threshold` — both read the same constant, so it passed
/// whatever the constant was. A test that cannot fail is worse than no test; the drift that
/// can actually happen is between the code and the file an operator reads.
#[test]
fn the_documented_threshold_matches_the_code() {
    let example = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("repo root")
            .join("config.example/tuning.toml"),
    )
    .expect("read config.example/tuning.toml");
    let documented = example
        .lines()
        .find_map(|l| l.trim().strip_prefix("# fuzzy_threshold = "))
        .expect("config.example must document fuzzy_threshold")
        .trim()
        .parse::<f64>()
        .expect("documented threshold must be a number");
    assert_eq!(
        documented,
        EditConfig::DEFAULT_FUZZY_THRESHOLD,
        "config.example says {documented} and the code uses {}",
        EditConfig::DEFAULT_FUZZY_THRESHOLD
    );
}

/// Fuzzy anchor matching is on by default, following `oh-my-pi`. Turning it off would
/// reinstate the failure mode that accounted for a large share of four runs' rejected edits.
#[test]
fn fuzzy_anchor_matching_is_on_by_default() {
    let edit = EditConfig::default();
    assert!(
        edit.fuzzy_match,
        "exact-only matching was measured as worse"
    );
    assert!(
        (0.9..=1.0).contains(&edit.fuzzy_threshold),
        "a threshold outside 0.9..=1.0 either rejects everything or edits the wrong place: {}",
        edit.fuzzy_threshold
    );
}

/// A default that a run cannot use is not a default. `hash_length` outside
/// `HASH_LENGTH_MIN..=HASH_LENGTH_MAX` fails `validate`, which would reject the config at
/// load and leave the run with no edit tooling at all.
#[test]
fn the_default_hash_length_is_inside_its_own_bounds() {
    let length = HashlineConfig::default().hash_length;
    assert!(
        (HashlineConfig::HASH_LENGTH_MIN..=HashlineConfig::HASH_LENGTH_MAX).contains(&length),
        "default hash_length {length} is outside {}..={}",
        HashlineConfig::HASH_LENGTH_MIN,
        HashlineConfig::HASH_LENGTH_MAX
    );
}

/// Both coding paths must agree. The divergence is what caused this, not the value.
#[test]
fn tuning_is_the_single_source_for_hashline() {
    let tuning = CoderTuning::default();
    assert_eq!(
        tuning.run_config().hashline,
        tuning.hashline,
        "a path that hardcodes its own HashlineConfig can silently disagree with the other"
    );
}
