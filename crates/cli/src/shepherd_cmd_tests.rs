//! Split from `shepherd_cmd.rs` for module-health boundaries.

use super::actions::{CleanAction, FailureAction, next_clean_action, next_failure_action};
use super::prompts::{cold_review_prompt, kickback_prompt, note};
use super::record::{self, ShepherdFact};
use super::*;

fn test_config(root: PathBuf) -> Config {
    Config {
        root,
        repository: None,
        check_names: Vec::new(),
        daemon: String::new(),
        project: String::new(),
        base: "main".into(),
        profile: String::new(),
        max_kickbacks: 2,
        cold_reviews: 2,
        cold_turns: 60,
        max_concurrent: 2,
        poll: 120,
    }
}
#[test]
fn parser_is_platform_specific_and_preserves_step_failure() {
    self_test().unwrap()
}
#[test]
fn preexisting_note_is_bounded() {
    let set = (0..11).map(|i| format!("j|{i}")).collect();
    assert!(note(&set).lines().count() <= 12)
}

#[test]
fn selected_checks_filter_by_job_name() {
    let cfg = Config {
        root: PathBuf::new(),
        repository: None,
        check_names: vec!["test (windows-latest)".into()],
        daemon: String::new(),
        project: String::new(),
        base: String::new(),
        profile: String::new(),
        max_kickbacks: 0,
        cold_reviews: 0,
        cold_turns: 0,
        max_concurrent: 0,
        poll: 0,
    };
    assert!(check_selected(&cfg, "test (windows-latest)|crate::test"));
    assert!(!check_selected(&cfg, "test (ubuntu-latest)|crate::test"));
}

#[test]
fn missing_selected_check_is_not_success() {
    let rows = vec![json!({"name":"test (ubuntu)","state":"SUCCESS"})];
    assert_eq!(check_status(&["test (windows)".into()], &rows), "pending");
}

#[test]
fn shepherd_config_rejects_unknown_coding_project() {
    let mut topology = liberado_config::Topology::default();
    topology
        .shepherd
        .projects
        .push(liberado_config::ShepherdProjectConfig {
            name: "example".into(),
            repository: "owner/repo".into(),
            coding_project: "missing".into(),
            base_branch: "main".into(),
            profile: "coding-unattended".into(),
            check_names: Vec::new(),
            max_kickbacks: None,
            cold_reviews: None,
            cold_review_max_turns: None,
            max_concurrent_goals: None,
            poll_seconds: None,
        });
    assert!(
        validate_shepherd_topology(&topology)
            .unwrap_err()
            .to_string()
            .contains("unknown coding_project")
    );
}

/// A minimal valid shepherd project, for the validation-branch tests below.
fn valid_project(name: &str) -> liberado_config::ShepherdProjectConfig {
    liberado_config::ShepherdProjectConfig {
        name: name.into(),
        repository: "owner/repo".into(),
        coding_project: "liberado".into(),
        base_branch: "main".into(),
        profile: "coding-unattended".into(),
        check_names: vec!["test".into()],
        max_kickbacks: None,
        cold_reviews: None,
        cold_review_max_turns: None,
        max_concurrent_goals: None,
        poll_seconds: None,
    }
}

fn topology_with(project: liberado_config::ShepherdProjectConfig) -> liberado_config::Topology {
    let mut topology = liberado_config::Topology::default();
    // The project must be declared in the application `[projects]` list too, or validation
    // fails on the unknown-coding-project check before reaching the branch under test.
    topology.projects.push(liberado_config::ProjectConfig {
        name: project.coding_project.clone(),
        root: PathBuf::from("/tmp/project"),
        write_class: liberado_common::WriteClass::AgentWritable,
        enabled: true,
        preflight: Default::default(),
    });
    topology.shepherd.projects.push(project);
    topology
}

fn rejects(project: liberado_config::ShepherdProjectConfig, needle: &str) {
    let error = validate_shepherd_topology(&topology_with(project))
        .unwrap_err()
        .to_string();
    assert!(error.contains(needle), "expected {needle:?} in {error:?}");
}

/// Every invalid shape is refused with a message that names the problem — a shepherd that
/// silently accepts a broken topology would mislabel PRs instead.
#[test]
fn shepherd_config_rejects_every_invalid_shape() {
    rejects(valid_project(""), "name must not be empty");
    rejects(
        liberado_config::ShepherdProjectConfig {
            name: "dupe".into(),
            repository: "not-owner-repo".into(),
            ..valid_project("dupe")
        },
        "OWNER/REPOSITORY",
    );
    rejects(
        liberado_config::ShepherdProjectConfig {
            base_branch: "  ".into(),
            ..valid_project("blank-base")
        },
        "base_branch and profile must not be empty",
    );
    rejects(
        liberado_config::ShepherdProjectConfig {
            max_concurrent_goals: Some(0),
            ..valid_project("zero-concurrent")
        },
        "must be greater than zero",
    );
    rejects(
        liberado_config::ShepherdProjectConfig {
            poll_seconds: Some(0),
            ..valid_project("zero-poll")
        },
        "must be greater than zero",
    );
    rejects(
        liberado_config::ShepherdProjectConfig {
            check_names: vec![String::new()],
            ..valid_project("empty-check")
        },
        "non-empty and unique",
    );
}

#[test]
fn shepherd_config_rejects_duplicate_project_names() {
    let mut topology = topology_with(valid_project("dupe"));
    topology.shepherd.projects.push(valid_project("dupe"));
    let error = validate_shepherd_topology(&topology)
        .unwrap_err()
        .to_string();
    assert!(error.contains("duplicate shepherd project name"), "{error}");
}

#[test]
fn shepherd_config_accepts_a_valid_project() {
    assert!(validate_shepherd_topology(&topology_with(valid_project("ok"))).is_ok());
}

// ── select_shepherd_project ────────────────────────────────────────

#[test]
fn project_selection_follows_the_three_way_rule() {
    let one = valid_project("one");
    let two = valid_project("two");
    // An explicit name wins and must exist.
    let both = [one.clone(), two.clone()];
    let picked = select_shepherd_project(Some("two"), &both)
        .unwrap()
        .unwrap();
    assert_eq!(picked.name, "two");
    assert!(
        select_shepherd_project(Some("nope"), std::slice::from_ref(&one))
            .unwrap_err()
            .contains("unknown shepherd project")
    );
    // A single configured project auto-applies.
    let single = [one.clone()];
    let picked = select_shepherd_project(None, &single).unwrap().unwrap();
    assert_eq!(picked.name, "one");
    // Several without a name is an error, not a guess.
    let pair = [one, two];
    assert!(
        select_shepherd_project(None, &pair)
            .unwrap_err()
            .contains("multiple")
    );
    // None configured means the environment defaults apply.
    assert!(select_shepherd_project(None, &[]).unwrap().is_none());
}

// ── apply_project / state ───────────────────────────────────────────

#[test]
fn apply_project_copies_every_field() {
    let mut cfg = test_config(PathBuf::from("/tmp/root"));
    let project = liberado_config::ShepherdProjectConfig {
        name: "p".into(),
        repository: "owner/repo".into(),
        coding_project: "proj".into(),
        base_branch: "dev".into(),
        profile: "prof".into(),
        check_names: vec!["a".into(), "b".into()],
        max_kickbacks: Some(1),
        cold_reviews: Some(3),
        cold_review_max_turns: Some(9),
        max_concurrent_goals: Some(4),
        poll_seconds: Some(30),
    };
    cfg.apply_project(&project);
    assert_eq!(cfg.repository.as_deref(), Some("owner/repo"));
    assert_eq!(cfg.check_names, vec!["a", "b"]);
    assert_eq!(cfg.project, "proj");
    assert_eq!(cfg.base, "dev");
    assert_eq!(cfg.profile, "prof");
    assert_eq!(cfg.max_kickbacks, 1);
    assert_eq!(cfg.cold_reviews, 3);
    assert_eq!(cfg.cold_turns, 9);
    assert_eq!(cfg.max_concurrent, 4);
    assert_eq!(cfg.poll, 30);
}

#[test]
fn state_lives_under_the_shepherd_dir() {
    let cfg = test_config(PathBuf::from("/tmp/root"));
    assert_eq!(cfg.state(), PathBuf::from("/tmp/root/.liberado/shepherd"));
}

// ── reset_baselines ─────────────────────────────────────────────────

#[test]
fn reset_baselines_removes_only_json_caches() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    let dir = cfg.state().join("baselines");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("abc.json"), "{}").unwrap();
    fs::write(dir.join("keep.txt"), "x").unwrap();
    reset_baselines(&cfg).unwrap();
    assert!(!dir.join("abc.json").exists(), "json cache must be removed");
    assert!(
        dir.join("keep.txt").exists(),
        "non-json files are not baselines"
    );
}

// ── Config::get ─────────────────────────────────────────────────────

/// `get` reads its environment variable, falling back to the default when unset. The var name
/// is unique to this test so the set/clear pair cannot be observed by another test.
#[test]
fn config_get_reads_env_with_a_default() {
    let key = "SHEPHERD_TEST_GET_9f2c7d";
    // Edition 2024 marks these unsafe: the pair is scoped to this test with a unique key.
    unsafe { std::env::set_var(key, "from-env") };
    assert_eq!(Config::get(key, "fallback"), "from-env");
    unsafe { std::env::remove_var(key) };
    assert_eq!(Config::get(key, "fallback"), "fallback");
}

// ── seed (dry mode) ────────────────────────────────────────────────

/// Dry mode parses and validates the task file but never talks to the daemon.
#[test]
fn seed_in_dry_mode_parses_tasks_without_starting_goals() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    let task = temp.path().join("tasks.txt");
    fs::write(&task, "# comment\n\nfirst task\nsecond task\n\n").unwrap();
    assert!(seed(&cfg, &task, true).is_ok());
    // A missing file is still an error in dry mode.
    assert!(seed(&cfg, &temp.path().join("nope.txt"), true).is_err());
}

#[test]
fn tick_idle_gates_pending_and_none_but_not_settled() {
    assert!(tick_idle("pending"), "pending CI is not a settled signal");
    assert!(tick_idle("none"), "no CI run is not a settled signal");
    assert!(!tick_idle("completed"), "settled CI lets tick proceed");
}

/// The baseline cache is read without touching the network: a `baselines/<short>.json` file
/// written by a previous run is the whole answer.
#[test]
fn baseline_reads_the_cached_failure_set() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    let sha = "0123456789abcdef";
    let dir = cfg.state().join("baselines");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("{}.json", &sha[..12])),
        serde_json::to_vec(&json!({
            "base_sha": sha,
            "failures": ["job|test::a", "job|step:Lint"],
            "provenance": "cache",
        }))
        .unwrap(),
    )
    .unwrap();
    let (failures, provenance) = baseline(&cfg, sha).unwrap();
    assert_eq!(provenance, "cache");
    assert!(failures.contains("job|test::a"), "{failures:?}");
    assert!(failures.contains("job|step:Lint"), "{failures:?}");
}

#[test]
fn settled_review_labels_only_on_success_and_preserves_dry_run_state() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    let mut pr = Pr {
        number: 42,
        title: "test".into(),
        branch: "test".into(),
        base_sha: String::new(),
        head_sha: String::new(),
        url: String::new(),
        labels: Vec::new(),
    };
    let path = pending(&cfg, pr.number);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, r#"{"session_id":"one","round":1}"#).unwrap();
    let mut labels = Vec::new();
    assert_eq!(
        settle_with(
            &cfg,
            &mut pr,
            false,
            |_| Some("failed".into()),
            |_, label| labels.push(label)
        )
        .unwrap(),
        ReviewTransition::Failed
    );
    assert!(!path.exists());
    assert!(labels.is_empty());

    fs::write(&path, r#"{"session_id":"two","round":1}"#).unwrap();
    assert_eq!(
        settle_with(
            &cfg,
            &mut pr,
            false,
            |_| Some("succeeded".into()),
            |_, label| labels.push(label)
        )
        .unwrap(),
        ReviewTransition::Labeled
    );
    assert!(!path.exists());
    assert_eq!(labels, ["shepherd:review-1"]);

    fs::write(&path, r#"{"session_id":"three","round":2}"#).unwrap();
    assert_eq!(
        settle_with(
            &cfg,
            &mut pr,
            true,
            |_| Some("succeeded".into()),
            |_, label| labels.push(label)
        )
        .unwrap(),
        ReviewTransition::Labeled
    );
    assert!(path.exists());
    assert_eq!(labels, ["shepherd:review-1"]);
}

#[test]
fn prompts_keep_unattended_guardrails() {
    let cfg = test_config(PathBuf::new());
    let pr = Pr {
        number: 1,
        title: "title".into(),
        branch: "branch".into(),
        base_sha: String::new(),
        head_sha: String::new(),
        url: String::new(),
        labels: Vec::new(),
    };
    let failures = BTreeSet::from(["test|case".into()]);
    let kickback = kickback_prompt(&pr, &failures, &BTreeSet::new());
    assert!(kickback.contains("Reproduce a new failure locally before changing anything"));
    assert!(kickback.contains("Do not delete, skip, or `#[ignore]` a test"));
    let review = cold_review_prompt(&cfg, &pr, 1, &BTreeSet::new());
    assert!(review.contains("Real, Exaggerated, or Hallucinated"));
    assert!(review.contains("Run it both ways"));
}

// ── parse_failure_set ───────────────────────────────────────────────

/// A rustc error with a diagnostic code (`error[E0123]`) is a step failure, folded into the
/// result as `job|step:<step>` — the self-test's plain `error:` is only one spelling.
#[test]
fn failure_set_recognises_diagnostic_codes() {
    let set = parse_failure_set("test (ubuntu-latest)\tLint\terror[E0123]: unresolved import\n");
    assert!(set.contains("test (ubuntu-latest)|step:Lint"), "{set:?}");
}

/// `error: could not compile` (the old cargo spelling, no bracket code) is a step failure too.
#[test]
fn failure_set_recognises_could_not_compile() {
    let set = parse_failure_set(
        "clippy\tLint\terror: could not compile `x` (due to 3 previous errors)\n",
    );
    assert!(set.contains("clippy|step:Lint"), "{set:?}");
}

/// A step whose named test failed must not ALSO be reported as a bare step failure — that
/// would double-count one failure.
#[test]
fn failure_set_does_not_double_count_named_tests() {
    let set = parse_failure_set("test (ubuntu-latest)\tTests\tX test crate::case ... FAILED\n");
    assert!(set.contains("test (ubuntu-latest)|crate::case"), "{set:?}");
    assert!(!set.contains("test (ubuntu-latest)|step:Tests"), "{set:?}");
}

/// Malformed rows (fewer than the three tab-separated columns) are skipped, not fatal.
#[test]
fn failure_set_skips_short_rows() {
    let set = parse_failure_set("only-two-columns\tignored\ngarbage\n");
    assert!(set.is_empty(), "{set:?}");
}

// ── check_status ────────────────────────────────────────────────────

fn row(name: &str, state: &str) -> serde_json::Value {
    json!({ "name": name, "state": state })
}

/// No checks reported yet reads as "none" — the PR is neither green nor red, just unreported.
#[test]
fn check_status_none_when_no_rows() {
    assert_eq!(check_status(&[], &[]), "none");
}

/// All passing → success; any pending/queued/in-progress state → pending (never success
/// early); any failure/error state → failure.
#[test]
fn check_status_aggregates_states() {
    let rows = vec![row("a", "SUCCESS"), row("b", "success")];
    assert_eq!(check_status(&[], &rows), "success");
    let rows = vec![row("a", "SUCCESS"), row("b", "in_progress")];
    assert_eq!(check_status(&[], &rows), "pending");
    let rows = vec![row("a", "SUCCESS"), row("b", "failure")];
    assert_eq!(check_status(&[], &rows), "failure");
    let rows = vec![row("a", "queued"), row("b", "timed_out")];
    assert_eq!(check_status(&[], &rows), "pending");
}

/// An empty check-name filter means "all reported checks" — the full row set is the gate.
#[test]
fn check_status_with_no_filter_uses_all_rows() {
    let rows = vec![row("a", "SUCCESS")];
    assert_eq!(check_status(&[], &rows), "success");
}

// ── Pr ──────────────────────────────────────────────────────────────

fn pr(labels: &[&str]) -> Pr {
    Pr {
        number: 7,
        title: "t".into(),
        branch: "b".into(),
        base_sha: String::new(),
        head_sha: String::new(),
        url: String::new(),
        labels: labels.iter().map(|s| s.to_string()).collect(),
    }
}

/// `has` is exact-label matching; `count` counts the numbered kickback labels; `terminal` is
/// ready-or-blocked.
#[test]
fn pr_label_helpers() {
    let p = pr(&["shepherd:kickback-1", "shepherd:kickback-2", READY]);
    assert!(p.has("shepherd:kickback-1"));
    assert!(!p.has("shepherd:kickback-3"));
    assert_eq!(p.count("shepherd:kickback-"), 2);
    assert!(p.terminal());
    assert!(!pr(&["shepherd:kickback-1"]).terminal());
    assert!(pr(&[BLOCKED]).terminal());
}

// ── review_transition ───────────────────────────────────────────────

/// Every status the daemon reports maps to exactly one transition; an unknown status fails
/// closed (the review is treated as failed rather than silently passing).
#[test]
fn review_transition_covers_every_status() {
    for waiting in [
        None,
        Some("running"),
        Some("pending"),
        Some("starting"),
        Some("active"),
        Some("parked"),
    ] {
        assert_eq!(
            review_transition(waiting),
            ReviewTransition::Waiting,
            "{waiting:?}"
        );
    }
    assert_eq!(
        review_transition(Some("succeeded")),
        ReviewTransition::Labeled
    );
    for failed in [Some("failed"), Some("cancelled"), Some("lost")] {
        assert_eq!(
            review_transition(failed),
            ReviewTransition::Failed,
            "{failed:?}"
        );
    }
}

// ── note ────────────────────────────────────────────────────────────

#[test]
fn note_is_empty_for_no_preexisting_failures() {
    assert_eq!(note(&BTreeSet::new()), "");
}

// ── parse_goal_status ───────────────────────────────────────────────

/// The status lives either at the top level or under `session`, lowercase either way. (A bare
/// status string is not a shape the daemon emits.)
#[test]
fn goal_status_reads_top_level_or_session() {
    assert_eq!(
        parse_goal_status(&json!({"status": "Running"})),
        Some("running".into())
    );
    assert_eq!(
        parse_goal_status(&json!({"session": {"status": "succeeded"}})),
        Some("succeeded".into())
    );
    assert_eq!(parse_goal_status(&json!({})), None);
    assert_eq!(parse_goal_status(&json!("running")), None);
}

#[test]
fn limits_default_when_the_environment_says_nothing() {
    let get = |_key: &str| None;
    let limits = Limits::from_reader(get).unwrap();
    assert_eq!(
        limits,
        Limits {
            max_kickbacks: 2,
            cold_reviews: 2,
            cold_turns: 60,
            max_concurrent: 2,
            poll: 120,
        }
    );
}

#[test]
fn limits_read_valid_overrides() {
    let get = |key: &str| match key {
        "SHEPHERD_MAX_KICKBACKS" => Some("5".into()),
        "SHEPHERD_POLL_SECONDS" => Some("30".into()),
        _ => None,
    };
    let limits = Limits::from_reader(get).unwrap();
    assert_eq!(limits.max_kickbacks, 5);
    assert_eq!(limits.poll, 30);
    assert_eq!(limits.cold_reviews, 2, "unset keys keep their default");
}

#[test]
fn limits_name_the_key_of_a_malformed_value() {
    let get = |key: &str| match key {
        "SHEPHERD_COLD_REVIEWS" => Some("many".into()),
        _ => None,
    };
    let error = Limits::from_reader(get).unwrap_err();
    assert!(
        error.contains("SHEPHERD_COLD_REVIEWS") && error.contains("many"),
        "the error must name the key and the bad value: {error}"
    );
}

#[test]
fn failure_escalation_reruns_once_then_kicks_then_blocks() {
    // First sighting: rerun CI before doing anything else.
    assert_eq!(
        next_failure_action(false, 0, 2, || true),
        FailureAction::Rerun
    );
    // A rerun already spent and the kickback cap reached: blocked, no matter the slots.
    assert_eq!(
        next_failure_action(true, 2, 2, || true),
        FailureAction::Blocked
    );
    // Cap not reached but every budget slot busy: wait.
    assert_eq!(
        next_failure_action(true, 0, 2, || false),
        FailureAction::WaitForSlot
    );
    // Rerun spent, cap not reached, slot free: start a kickback goal.
    assert_eq!(
        next_failure_action(true, 1, 2, || true),
        FailureAction::Kickback
    );
}

#[test]
fn clean_prs_ready_after_cap_else_review_when_a_slot_frees_up() {
    assert_eq!(next_clean_action(2, 2, || true), CleanAction::Ready);
    assert_eq!(next_clean_action(1, 3, || false), CleanAction::WaitForSlot);
    assert_eq!(
        next_clean_action(1, 3, || true),
        CleanAction::Review { round: 2 },
        "the review round continues from the reviews already done"
    );
}

#[test]
fn apply_project_overrides_only_the_fields_it_declares() {
    let mut cfg = test_config(PathBuf::new());
    cfg.daemon = "http://env-daemon".into();
    let project = liberado_config::ShepherdProjectConfig {
        name: "p".into(),
        repository: "owner/repo".into(),
        coding_project: "proj".into(),
        base_branch: "trunk".into(),
        profile: "coding".into(),
        check_names: vec!["test".into()],
        max_kickbacks: Some(7),
        cold_reviews: None,
        cold_review_max_turns: None,
        max_concurrent_goals: None,
        poll_seconds: None,
    };
    cfg.apply_project(&project);
    assert_eq!(cfg.repository.as_deref(), Some("owner/repo"));
    assert_eq!(cfg.project, "proj");
    assert_eq!(cfg.base, "trunk");
    assert_eq!(cfg.profile, "coding");
    assert_eq!(cfg.max_kickbacks, 7);
    assert_eq!(cfg.check_names, vec!["test".to_string()]);
    // Fields the project leaves unset keep their environment-derived values.
    assert_eq!(cfg.cold_reviews, 2);
    assert_eq!(cfg.cold_turns, 60);
    assert_eq!(cfg.max_concurrent, 2);
    assert_eq!(cfg.poll, 120);
    assert_eq!(
        cfg.daemon, "http://env-daemon",
        "daemon is never project-set"
    );
}

fn argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn invocation_parses_every_documented_shape() {
    assert!(matches!(
        parse_invocation(&argv(&["--self-test"])).unwrap().mode,
        Invocation::SelfTest
    ));
    let config_check =
        parse_invocation(&argv(&["config", "check", "--project", "liberado"])).unwrap();
    assert_eq!(
        config_check.mode,
        Invocation::ConfigCheck {
            project: Some("liberado".into())
        }
    );
    let seed_drive = parse_invocation(&argv(&["--seed", "tasks.txt", "--dry-run"])).unwrap();
    assert_eq!(
        seed_drive.mode,
        Invocation::Drive {
            once: false,
            watch: false
        }
    );
    assert_eq!(seed_drive.seed, Some(PathBuf::from("tasks.txt")));
    assert!(seed_drive.dry_run);
}

#[test]
fn invocation_demands_a_mode_before_doing_anything() {
    let error = parse_invocation(&argv(&["--dry-run"])).unwrap_err();
    assert!(error.contains("--once|--watch|--seed"), "{error}");
}

#[test]
fn invocation_rejects_config_without_check() {
    let error = parse_invocation(&argv(&["config"])).unwrap_err();
    assert!(error.contains("config check"), "{error}");
}

#[test]
fn invocation_carries_the_secondary_flags() {
    let parsed = parse_invocation(&argv(&[
        "--once",
        "--reset-baselines",
        "--project",
        "other",
    ]))
    .unwrap();
    assert!(parsed.reset_baselines);
    assert!(!parsed.dry_run);
    assert_eq!(parsed.project.as_deref(), Some("other"));
    assert_eq!(
        parsed.mode,
        Invocation::Drive {
            once: true,
            watch: false
        }
    );
}

fn sample_pr(number: u64, head_sha: &str) -> Pr {
    Pr {
        number,
        title: "feat".into(),
        branch: "feat/x".into(),
        base_sha: "bbbb".into(),
        head_sha: head_sha.into(),
        url: format!("https://github.com/ForrestThump/liberado/pull/{number}"),
        labels: Vec::new(),
    }
}

#[test]
fn shepherd_dry_run_writes_no_task_ledger() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    let pr = sample_pr(12, "aaa111");
    let recorded = record::record_facts(
        &cfg,
        &pr,
        true,
        &[ShepherdFact::Ci {
            github_run_id: Some(9),
            state: "success".into(),
            failures: Vec::new(),
        }],
    )
    .unwrap();
    assert!(recorded.is_none());
    assert!(
        !temp.path().join(".liberado/tasks").exists(),
        "dry-run must not create the durable ledger root"
    );
}

#[test]
fn shepherd_repeated_facts_are_idempotent_and_survive_reload() {
    let temp = tempfile::tempdir().unwrap();
    let mut cfg = test_config(temp.path().to_path_buf());
    cfg.repository = Some("ForrestThump/liberado".into());
    let pr = sample_pr(15, "ccc222ddd333");
    let facts = [
        ShepherdFact::Ci {
            github_run_id: Some(88),
            state: "failure".into(),
            failures: vec!["job|test".into()],
        },
        ShepherdFact::Rerun {
            github_run_id: Some(88),
        },
        ShepherdFact::Repair {
            goal_id: Some("goal-a".into()),
            reason: "1 new CI failures".into(),
            kick: 1,
        },
    ];
    let first = record::record_facts(&cfg, &pr, false, &facts)
        .unwrap()
        .expect("live record");
    assert_eq!(first.rerun_count, 1);
    assert_eq!(first.repair_count, 1);
    assert_eq!(first.pull_request_number, Some(15));
    assert_eq!(first.head_revision.as_deref(), Some("ccc222ddd333"));
    assert_eq!(first.controller.as_deref(), Some("liberado-shepherd"));
    assert!(!first.is_pr_ready());
    let first_len = record::open_recorded(&cfg, &pr).unwrap().events().len();

    let second = record::record_facts(&cfg, &pr, false, &facts)
        .unwrap()
        .expect("repeat record");
    assert_eq!(second.rerun_count, first.rerun_count);
    assert_eq!(second.repair_count, first.repair_count);
    let second_len = record::open_recorded(&cfg, &pr).unwrap().events().len();
    assert_eq!(second_len, first_len);

    let reloaded = record::open_recorded(&cfg, &pr).unwrap();
    let restored = reloaded.project().unwrap();
    assert_eq!(restored.rerun_count, 1);
    assert_eq!(restored.repair_count, 1);
    assert_eq!(restored.github_run_id, Some(88));
    assert_eq!(reloaded.events().len(), first_len);
}

#[test]
fn shepherd_ready_binds_head_ci_and_review() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    let pr = sample_pr(3, "fff444");
    let ready = record::record_facts(
        &cfg,
        &pr,
        false,
        &[
            ShepherdFact::Ci {
                github_run_id: Some(21),
                state: "success".into(),
                failures: Vec::new(),
            },
            ShepherdFact::ReviewApproved { round: 2 },
            ShepherdFact::Ready {
                github_run_id: Some(21),
                review_round: 2,
            },
        ],
    )
    .unwrap()
    .expect("ready record");
    assert!(ready.is_pr_ready());
    assert_eq!(ready.ready_evidence.as_ref().unwrap().head_sha, "fff444");
    assert_eq!(
        ready.ready_evidence.as_ref().unwrap().ci_github_run_id,
        Some(21)
    );
    assert_eq!(ready.status, liberado_coder_core::TaskStatus::Completed);
}
