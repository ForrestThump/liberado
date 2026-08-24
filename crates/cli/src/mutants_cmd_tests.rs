//! Split from `mutants_cmd.rs` for module-health boundaries.

use super::*;

fn init_git_repo(root: &Path) {
    for (dir, name) in [
        ("crates/alpha", "liberado-alpha"),
        ("crates/beta", "liberado-beta"),
    ] {
        fs::create_dir_all(root.join(dir)).unwrap();
        fs::write(
            root.join(dir).join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\n\n[package.metadata.liberado]\nrole = \"kernel\"\n"
            ),
        )
        .unwrap();
        fs::write(
            root.join(dir).join("lib.rs"),
            "pub fn value() -> i32 { 1 }\n",
        )
        .unwrap();
    }
    run_git(root, &["init"]);
    run_git(root, &["config", "user.email", "test@example.com"]);
    run_git(root, &["config", "user.name", "Test"]);
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-m", "initial"]);
}

fn run_git(root: &Path, args: &[&str]) {
    // `std_command`, not raw `Command::new`: the subprocess rule scans this file as
    // production code (it has no `#[cfg(test)]` marker of its own), and the helper
    // nulls the child's stdin anyway.
    let status = std_command("git")
        .args(args)
        .current_dir(root)
        .status()
        .expect("git command");
    assert!(status.success(), "git {:?} failed", args);
}

#[test]
fn ingest_counts_from_outcomes_json() {
    let outcomes: OutcomesFile = serde_json::from_str(
        r#"{
  "caught": 3,
  "missed": 1,
  "timeout": 0,
  "unviable": 2,
  "cargo_mutants_version": "27.1.0"
}"#,
    )
    .unwrap();
    let counts = outcomes.counts();
    assert_eq!(counts.viable, 4);
    assert_eq!(counts.caught, 3);
    assert_eq!(counts.survived, 1);
    assert_eq!(counts.unviable, 2);
}

#[test]
fn package_from_outcomes_skips_baseline_row() {
    let package = package_from_outcomes_bytes(
        br#"{
  "outcomes": [
{"scenario": "Baseline"},
{"scenario": {"Mutant": {"package": "liberado-alpha"}}}
  ],
  "caught": 1,
  "missed": 0,
  "timeout": 0,
  "unviable": 0,
  "cargo_mutants_version": "27.1.0"
}"#,
    );
    assert_eq!(package, Some("liberado-alpha".into()));
}

#[test]
fn record_refuses_zero_viable_outcomes_so_a_crashed_run_cannot_shadow_the_last_campaign() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_git_repo(root);
    // A completed outcomes file from a run whose baseline build never happened: every
    // count zero. Recording it would append an all-zero row that the report treats as
    // the crate's newest campaign.
    fs::create_dir_all(root.join("mutants.out")).unwrap();
    fs::write(
        root.join(OUTCOMES_FILE),
        r#"{
  "caught": 0,
  "missed": 0,
  "timeout": 0,
  "unviable": 0,
  "cargo_mutants_version": "27.1.0"
}"#,
    )
    .unwrap();

    let outcome = record_campaign(root, Some("alpha"), None, RunProfile::Default).unwrap();
    assert!(matches!(outcome, RecordOutcome::SkippedIncomplete));
    assert!(
        load_ledger(root).unwrap().campaigns.is_empty(),
        "a zero-viable run must not append a ledger row"
    );
}

#[test]
fn ledger_append_preserves_prior_rows() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join(LEDGER_FILE),
        r#"{"schema":1,"campaigns":[{"package":"liberado-alpha","commit":null,"recorded_at":"2026-07-29","scope":"package","source":"markdown-seed","counts":{"viable":1,"caught":1,"survived":0,"timeout":0,"unviable":0}}]}"#,
    )
    .unwrap();
    append_campaign(
        root,
        Campaign {
            package: "liberado-alpha".into(),
            commit: Some("abc123".into()),
            recorded_at: "2026-08-21".into(),
            command: Some("cargo mutants -p liberado-alpha".into()),
            tool_version: Some("27.1.0".into()),
            scope: "package".into(),
            counts: Counts {
                viable: 4,
                caught: 4,
                survived: 0,
                timeout: 0,
                unviable: 0,
            },
            source: None,
        },
    )
    .unwrap();
    let ledger = load_ledger(root).unwrap();
    assert_eq!(ledger.campaigns.len(), 2);
    assert!(ledger.campaigns[0].commit.is_none());
    assert_eq!(ledger.campaigns[1].commit.as_deref(), Some("abc123"));
}

#[test]
fn report_groups_never_historical_and_drift() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_git_repo(root);
    let base = current_commit(root).unwrap();
    fs::write(
        root.join("crates/beta/lib.rs"),
        "pub fn value() -> i32 { 2 }\n",
    )
    .unwrap();
    run_git(root, &["add", "crates/beta/lib.rs"]);
    run_git(root, &["commit", "-m", "change beta"]);

    let ledger = Ledger {
        schema: 1,
        campaigns: vec![
            Campaign {
                package: "liberado-alpha".into(),
                commit: None,
                recorded_at: "2026-07-29".into(),
                command: None,
                tool_version: Some("27.1.0".into()),
                scope: "package".into(),
                counts: Counts {
                    viable: 10,
                    caught: 9,
                    survived: 1,
                    timeout: 0,
                    unviable: 0,
                },
                source: Some("markdown-seed".into()),
            },
            Campaign {
                package: "liberado-beta".into(),
                commit: Some(base),
                recorded_at: "2026-08-01".into(),
                command: Some("cargo mutants -p liberado-beta".into()),
                tool_version: Some("27.1.0".into()),
                scope: "package".into(),
                counts: Counts {
                    viable: 5,
                    caught: 4,
                    survived: 1,
                    timeout: 0,
                    unviable: 0,
                },
                source: None,
            },
        ],
    };
    let crates = crate_map_cmd::list_crates(root).unwrap();
    let health = build_health(root, &ledger, &crates, true).unwrap();
    assert!(health.never_campaigned.is_empty());
    assert_eq!(health.historical_only.len(), 1);
    assert_eq!(health.historical_only[0].dir, "alpha");
    assert_eq!(health.most_drift.len(), 1);
    assert_eq!(health.most_drift[0].dir, "beta");
    assert_eq!(health.most_drift[0].commits_since, Some(1));
}

#[test]
fn drift_marks_missing_ancestor() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_git_repo(root);
    let crates = crate_map_cmd::list_crates(root).unwrap();
    let ledger = Ledger {
        schema: 1,
        campaigns: vec![Campaign {
            package: "liberado-alpha".into(),
            commit: Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into()),
            recorded_at: "2026-08-01".into(),
            command: None,
            tool_version: None,
            scope: "package".into(),
            counts: Counts {
                viable: 1,
                caught: 1,
                survived: 0,
                timeout: 0,
                unviable: 0,
            },
            source: None,
        }],
    };
    let health = build_health(root, &ledger, &crates, true).unwrap();
    assert_eq!(health.most_drift.len(), 1);
    assert_eq!(
        health.most_drift[0].drift_note.as_deref(),
        Some("commit not in this history")
    );
}

#[test]
fn build_mutants_command_uses_longer_timeout_for_cli() {
    let cli = build_mutants_command("liberado-cli", RunProfile::Default);
    assert!(cli.contains("--timeout 120"));
    assert!(cli.contains("--minimum-test-timeout 120"));

    let tui = build_mutants_command("liberado-tui", RunProfile::Default);
    assert!(tui.contains("--timeout 3.0"));
    assert!(tui.contains("--minimum-test-timeout 30"));

    let acp = build_mutants_command("liberado-acp-bridge", RunProfile::Default);
    assert!(acp.contains("--timeout 10.0"));
    assert!(acp.contains("--minimum-test-timeout 120"));

    // Entries added after cold-baseline timeout autopsies; each one here
    // means a crate whose unmutated test phase exceeded the 3s floor.
    let memory = build_mutants_command("liberado-memory-mcp", RunProfile::Default);
    assert!(memory.contains("--timeout 60"));
    let conversation = build_mutants_command("liberado-conversation-store", RunProfile::Default);
    assert!(conversation.contains("--timeout 60"));
    let core = build_mutants_command("liberado-coder-core", RunProfile::Default);
    assert!(core.contains("--timeout 90"));
}

#[test]
fn repo_mutants_ledger_parses() {
    let root = crate_map_cmd::repository_root().expect("repository root");
    let ledger = load_ledger(&root).expect("ledger should parse");
    assert_eq!(ledger.schema, 1);
    assert!(
        !ledger.campaigns.is_empty(),
        "seed ledger should not be empty"
    );
}
// ── render_report ───────────────────────────────────────────────────

fn entry(dir: &str, role: &str) -> CrateHealthEntry {
    CrateHealthEntry {
        dir: dir.into(),
        role: role.into(),
        latest_commit: None,
        commits_since: None,
        lines_changed: None,
        drift_note: None,
        latest_counts: None,
    }
}

#[test]
fn render_report_names_each_section_and_empty_state() {
    let health = HealthReport {
        never_campaigned: vec![entry("alpha", "kernel")],
        historical_only: vec![],
        most_drift: vec![],
    };
    let text = render_report(&health);
    assert!(text.starts_with("=== Mutants campaign health ===\n\n"));
    assert!(
        text.contains("Never campaigned (1):\n  alpha [kernel]\n"),
        "must list the crate with its role, got:\n{text}"
    );
    assert!(text.contains("Historical only — no commit SHA (0):\n  (none)\n"));
    assert!(text.contains("Most drift since last SHA campaign (0):\n  (none)\n"));
}

#[test]
fn render_report_historical_row_includes_counts_only_when_present() {
    let mut with_counts = entry("beta", "surface");
    with_counts.latest_counts = Some(Counts {
        viable: 5,
        caught: 4,
        survived: 1,
        timeout: 0,
        unviable: 0,
    });
    let health = HealthReport {
        never_campaigned: vec![],
        historical_only: vec![with_counts, entry("gamma", "pack")],
        most_drift: vec![],
    };
    let text = render_report(&health);
    assert!(
        text.contains("  beta [surface] — viable 5 caught 4 survived 1 timeout 0\n"),
        "counts must render on the row that has them, got:\n{text}"
    );
    assert_eq!(
        text.match_indices("  gamma [pack]\n").count(),
        1,
        "a row without counts must not gain the counts suffix"
    );
}

#[test]
fn render_report_drift_row_covers_note_lines_and_commit_fallbacks() {
    let mut noted = entry("alpha", "kernel");
    noted.drift_note = Some("commit not in this history".into());

    let mut full = entry("beta", "client");
    full.latest_commit = Some("abc123def456".into());
    full.commits_since = Some(3);
    full.lines_changed = Some("2 files changed, 9 insertions(+)".into());
    full.latest_counts = Some(Counts {
        viable: 7,
        caught: 7,
        survived: 0,
        timeout: 0,
        unviable: 0,
    });

    let mut blank_lines = entry("gamma", "store");
    blank_lines.lines_changed = Some(String::new());

    let mut no_sha = entry("delta", "tool");
    no_sha.commits_since = Some(1);

    let health = HealthReport {
        never_campaigned: vec![],
        historical_only: vec![],
        most_drift: vec![noted, full, blank_lines, no_sha],
    };
    let text = render_report(&health);

    assert!(
        text.contains("  alpha [kernel] — commit not in this history\n"),
        "drift note replaces the commit arithmetic, got:\n{text}"
    );
    assert!(text.contains(
        "  beta [client] — 3 commits since abc123def456 — 2 files changed, 9 insertions(+) — viable 7 caught 7 survived 0 timeout 0\n"
    ));
    assert!(
        text.contains("  gamma [store] — 0 commits since ? — 0 files changed"),
        "blank shortstat falls back to the zero-files wording, got:\n{text}"
    );
    assert!(text.contains("  delta [tool] — 1 commits since ? — 0 files changed"));
}
