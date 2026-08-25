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

/// A partial outcomes file (killed mid-campaign) must be refused even though
/// every count is plausible: accounted != declared total.
#[test]
fn record_refuses_partial_outcomes_below_the_declared_total() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_git_repo(root);
    fs::create_dir_all(root.join("mutants.out")).unwrap();
    fs::write(
        root.join(OUTCOMES_FILE),
        r#"{
  "total_mutants": 10,
  "caught": 3,
  "missed": 2,
  "timeout": 0,
  "unviable": 1,
  "cargo_mutants_version": "27.1.0"
}"#,
    )
    .unwrap();

    let outcome = record_campaign(root, Some("alpha"), None, RunProfile::Default).unwrap();
    assert!(matches!(outcome, RecordOutcome::SkippedIncomplete));
    assert!(load_ledger(root).unwrap().campaigns.is_empty());
}

/// A complete file records normally: caught+survived+timeout+unviable == total.
#[test]
fn record_accepts_outcomes_that_account_for_every_mutant() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_git_repo(root);
    fs::create_dir_all(root.join("mutants.out")).unwrap();
    fs::write(
        root.join(OUTCOMES_FILE),
        r#"{
  "total_mutants": 6,
  "caught": 4,
  "missed": 1,
  "timeout": 0,
  "unviable": 1,
  "cargo_mutants_version": "27.1.0"
}"#,
    )
    .unwrap();

    let outcome = record_campaign(root, Some("alpha"), None, RunProfile::Default).unwrap();
    assert!(matches!(outcome, RecordOutcome::Appended { .. }));
    let ledger = load_ledger(root).unwrap();
    assert_eq!(ledger.campaigns.len(), 1);
    assert_eq!(ledger.campaigns[0].counts.survived, 1);
}

/// The zero-viable row is skipped when grouping for health/next: an older
/// crash row must not shadow the crate's real campaign.
#[test]
fn grouping_skips_zero_viable_rows_so_a_real_campaign_stays_visible() {
    let mk = |survived: u32, viable: u32| Campaign {
        package: "liberado-alpha".into(),
        commit: Some("0e14ecc1c7521034c9142782a0306861584acb29".into()),
        recorded_at: "2026-08-23".into(),
        command: None,
        tool_version: Some("27.1.0".into()),
        scope: "package".into(),
        counts: Counts {
            viable,
            caught: viable - survived,
            survived,
            timeout: 0,
            unviable: 0,
        },
        source: None,
    };
    let ledger = Ledger {
        schema: 1,
        campaigns: vec![mk(0, 0), mk(3, 5)],
    };
    let scratch = tempfile::tempdir().unwrap();
    save_ledger(scratch.path(), &ledger).unwrap();
    let ledger_on_disk = load_ledger(scratch.path()).unwrap();
    let grouped = package_campaigns_by_package(&ledger_on_disk);
    let rows = grouped.get("liberado-alpha").unwrap();
    assert_eq!(rows.len(), 1, "the zero-viable crash row is skipped");
    assert_eq!(rows[0].counts.survived, 3);
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

fn health_entry(dir: &str) -> CrateHealthEntry {
    CrateHealthEntry {
        dir: dir.into(),
        role: "kernel".into(),
        latest_commit: None,
        commits_since: None,
        lines_changed: None,
        drift_note: None,
        latest_counts: None,
    }
}

#[test]
fn empty_sections_render_none() {
    assert_eq!(
        render_never_campaigned(&[]),
        "\nNever campaigned (0):\n  (none)\n"
    );
    assert_eq!(
        render_historical_only(&[]),
        "\nHistorical only — no commit SHA (0):\n  (none)\n"
    );
    assert_eq!(
        render_most_drift(&[]),
        "\nMost drift since last SHA campaign (0):\n  (none)\n"
    );
}

#[test]
fn never_campaigned_lists_each_dir_and_role() {
    let entries = vec![health_entry("alpha"), health_entry("beta")];
    assert_eq!(
        render_never_campaigned(&entries),
        "\nNever campaigned (2):\n  alpha [kernel]\n  beta [kernel]\n"
    );
}

#[test]
fn historical_only_appends_counts_when_present_only() {
    let mut with_counts = health_entry("alpha");
    with_counts.latest_counts = Some(Counts {
        viable: 5,
        caught: 4,
        survived: 1,
        timeout: 0,
        unviable: 0,
    });
    let entries = vec![with_counts, health_entry("beta")];
    assert_eq!(
        render_historical_only(&entries),
        "\nHistorical only — no commit SHA (2):\n  \
         alpha [kernel] — viable 5 caught 4 survived 1 timeout 0\n  beta [kernel]\n"
    );
}

#[test]
fn most_drift_prefers_the_drift_note_over_commit_detail() {
    let mut noted = health_entry("alpha");
    noted.drift_note = Some("commit not in this history".into());
    let mut detailed = health_entry("beta");
    detailed.latest_commit = Some("abc123def456".into());
    detailed.commits_since = Some(2);
    detailed.lines_changed = Some(String::new());
    detailed.latest_counts = Some(Counts {
        viable: 1,
        caught: 0,
        survived: 1,
        timeout: 0,
        unviable: 0,
    });
    let entries = vec![noted, detailed];
    assert_eq!(
        render_most_drift(&entries),
        "\nMost drift since last SHA campaign (2):\n  \
         alpha [kernel] — commit not in this history\n  \
         beta [kernel] — 2 commits since abc123def456 — 0 files changed — \
         viable 1 caught 0 survived 1 timeout 0\n"
    );
}

// ── record_campaign: the remaining skip/append arms ────────────────────────────────

fn write_outcomes(root: &Path, body: &str) {
    fs::create_dir_all(root.join("mutants.out")).unwrap();
    fs::write(root.join(OUTCOMES_FILE), body).unwrap();
}

const FULL_OUTCOMES: &str = r#"{
  "total_mutants": 4,
  "outcomes": [
{"scenario": "Baseline"},
{"scenario": {"Mutant": {"package": "liberado-alpha"}}},
{"scenario": {"Mutant": {"package": "liberado-alpha"}}},
{"scenario": {"Mutant": {"package": "liberado-alpha"}}},
{"scenario": {"Mutant": {"package": "liberado-alpha"}}}
  ],
  "caught": 3,
  "missed": 1,
  "timeout": 0,
  "unviable": 0,
  "cargo_mutants_version": "27.1.0"
}"#;

#[test]
fn a_complete_campaign_appends_and_names_the_commit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_git_repo(root);
    write_outcomes(root, FULL_OUTCOMES);

    let outcome = record_campaign(
        root,
        None,
        Some("cargo mutants -p liberado-alpha --file x"),
        RunProfile::Default,
    )
    .unwrap();
    match outcome {
        RecordOutcome::Appended { package, commit } => {
            assert_eq!(package, "liberado-alpha");
            assert!(!commit.is_empty(), "the commit is recorded");
        }
        other => panic!("expected Appended, got {other:?}"),
    }
    let ledger = load_ledger(root).unwrap();
    assert_eq!(ledger.campaigns.len(), 1);
    // A `--file` command records file scope, not package scope.
    assert_eq!(ledger.campaigns[0].scope, "file");
    assert_eq!(
        ledger.campaigns[0].command.as_deref(),
        Some("cargo mutants -p liberado-alpha --file x")
    );
}

#[test]
fn a_partial_campaign_is_not_recorded() {
    // cargo-mutants writes outcomes incrementally: fewer accounted outcomes than the
    // declared total means the run died mid-campaign.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_git_repo(root);
    write_outcomes(
        root,
        r#"{
  "total_mutants": 9,
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
    let outcome = record_campaign(root, None, None, RunProfile::Default).unwrap();
    assert!(matches!(outcome, RecordOutcome::SkippedIncomplete));
    assert!(load_ledger(root).unwrap().campaigns.is_empty());
}

#[test]
fn an_outcomes_file_without_a_version_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_git_repo(root);
    write_outcomes(
        root,
        r#"{ "caught": 1, "missed": 0, "timeout": 0, "unviable": 0, "cargo_mutants_version": "" }"#,
    );
    let outcome = record_campaign(root, None, None, RunProfile::Default).unwrap();
    assert!(matches!(outcome, RecordOutcome::SkippedIncomplete));
}

#[test]
fn a_mistyped_outcomes_file_is_an_error_not_a_skip() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_git_repo(root);
    write_outcomes(root, "not json at all");
    assert!(record_campaign(root, None, None, RunProfile::Default).is_err());
}

#[test]
fn an_explicit_crate_dir_that_disagrees_with_outcomes_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_git_repo(root);
    write_outcomes(root, FULL_OUTCOMES);

    let outcome = record_campaign(root, Some("beta"), None, RunProfile::Default);
    let err = outcome.unwrap_err().to_string();
    assert!(
        err.contains("does not match crate directory"),
        "the mismatch must be named: {err}"
    );
}

// ── package_campaigns_by_package: the grouping rule ─────────────────────────────────

#[test]
fn grouping_keeps_only_viable_package_scope_rows() {
    let campaign = |package: &str, viable: u32| Campaign {
        package: package.into(),
        commit: None,
        recorded_at: "2026-08-24".into(),
        command: None,
        tool_version: None,
        scope: "package".into(),
        counts: Counts {
            viable,
            caught: viable,
            survived: 0,
            timeout: 0,
            unviable: 0,
        },
        source: None,
    };
    let workspace_row = Campaign {
        scope: "workspace".into(),
        ..campaign("liberado-alpha", 5)
    };
    let ledger = Ledger {
        schema: 1,
        campaigns: vec![
            campaign("liberado-alpha", 4),
            campaign("liberado-beta", 2),
            workspace_row,
            // A crashed/partial run from before the zero-viable refusal existed.
            campaign("liberado-gamma", 0),
        ],
    };

    let grouped = package_campaigns_by_package(&ledger);
    assert_eq!(grouped.len(), 2, "gamma's zero-viable row must not appear");
    assert_eq!(grouped["liberado-alpha"].len(), 1);
    assert_eq!(grouped["liberado-beta"].len(), 1);
}

/// A merge that unions both sides can paste a whole block twice (13
/// duplicates landed that way during the campaign). save_ledger must drop
/// exact duplicates so the on-disk artifact stays append-only AND
/// duplicate-free, whatever git history looked like mid-merge.
#[test]
fn save_ledger_drops_exact_duplicate_rows() {
    let root = tempfile::tempdir().unwrap();
    let mk = |pkg: &str, survived: u32| Campaign {
        package: pkg.to_string(),
        commit: Some("a".repeat(40)),
        recorded_at: "2026-08-24".to_string(),
        command: Some("cargo mutants -p x".to_string()),
        tool_version: Some("27.1.0".to_string()),
        scope: "package".to_string(),
        counts: Counts {
            viable: 10,
            caught: 10 - survived,
            survived,
            timeout: 0,
            unviable: 0,
        },
        source: None,
    };
    let ledger = Ledger {
        schema: 1,
        campaigns: vec![
            mk("liberado-a", 3),
            mk("liberado-b", 5),
            mk("liberado-a", 3), // exact duplicate of the first row
        ],
    };
    save_ledger(root.path(), &ledger).expect("save succeeds");

    let reloaded = load_ledger(root.path()).expect("reload");
    assert_eq!(reloaded.campaigns.len(), 2, "the duplicate is gone");
    assert_eq!(reloaded.campaigns[0].package, "liberado-a");
    assert_eq!(reloaded.campaigns[1].package, "liberado-b");

    // Distinct rows with equal survivors are NOT duplicates.
    let varied = Ledger {
        schema: 1,
        campaigns: vec![mk("liberado-x", 4), mk("liberado-y", 4)],
    };
    save_ledger(root.path(), &varied).expect("save succeeds");
    assert_eq!(load_ledger(root.path()).unwrap().campaigns.len(), 2);
}

// ── report/next flag parsing ─────────────────────────────────────────────────

#[test]
fn parse_include_all_accepts_nothing_or_all_only() {
    let none: Vec<String> = vec![];
    assert!(!run_support::parse_include_all(&mut none.iter().cloned(), "u").unwrap());

    let all = vec!["--all".to_string()];
    assert!(run_support::parse_include_all(&mut all.into_iter(), "u").unwrap());

    // A typo must be a usage error naming what was seen, never a silent
    // filtered run.
    let typo = vec!["--al".to_string()];
    let err = run_support::parse_include_all(
        &mut typo.into_iter(),
        "usage: liberado mutants report [--all]",
    )
    .unwrap_err();
    assert!(err.contains("--al") && err.contains("usage"), "{err}");
}
