use super::*;
use std::io::Write;

fn write_temp(name: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("liberado-mvl-oracle-unit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(body.as_bytes()).unwrap();
    f.flush().unwrap();
    path
}

#[test]
fn parse_args_requires_mvl() {
    let err = parse_oracle_args(["--execution", "x.jsonl"]).unwrap_err();
    assert!(err.contains("--mvl"), "{err}");
}

#[test]
fn parse_args_reads_honesty_file() {
    let truth = write_temp("truth.txt", "hit");
    let (mvl, opts) = parse_oracle_args([
        "--mvl",
        "run.mvl.jsonl",
        "--expected-content-shown",
        &format!("c1={}", truth.display()),
        "--kill-after-seq",
        "3",
    ])
    .unwrap();
    assert_eq!(mvl, PathBuf::from("run.mvl.jsonl"));
    assert_eq!(opts.expected_content_shown.get("c1").unwrap(), "hit");
    assert_eq!(opts.kill_after_seq, Some(3));
}

/// Every rule's wire string — the spelling foreign harnesses see in reports and pin their
/// tooling to.
#[test]
fn as_str_is_the_stable_wire_name_for_every_rule() {
    use ConformanceRule::*;
    assert_eq!(Reconstruction.as_str(), "reconstruction");
    assert_eq!(CrashSurvival.as_str(), "crash_survival");
    assert_eq!(Ordering.as_str(), "ordering");
    assert_eq!(
        SystemPromptRecoverable.as_str(),
        "system_prompt_recoverable"
    );
    assert_eq!(
        ToolCatalogueRecoverable.as_str(),
        "tool_catalogue_recoverable"
    );
    assert_eq!(ToolHonesty.as_str(), "tool_honesty");
    assert_eq!(WithdrawalVisible.as_str(), "withdrawal_visible");
    assert_eq!(JoinIntegrity.as_str(), "join_integrity");
    // ALL and as_str must agree: a rule added without a wire name would report as "".
    for rule in ConformanceRule::ALL {
        assert!(!rule.as_str().is_empty());
    }
}

fn report_with(status: VerdictStatus) -> ConformanceReport {
    ConformanceReport {
        mvl_path: String::new(),
        execution_path: None,
        verdicts: vec![RuleVerdict {
            rule: ConformanceRule::Reconstruction,
            status,
            detail: String::new(),
        }],
    }
}

#[test]
fn failed_returns_only_fail_verdicts() {
    // The `failed` body filters by `status == VerdictStatus::Fail`. cargo-mutants's
    // `replace with vec![]` and `==` -> `!=` mutations both make the function lose
    // the Fail signal: an empty return and an inverted predicate both report a
    // failing report as having zero failed verdicts. A mixed report with a Pass and
    // a Fail pins the filter: only the Fail must come back.
    let report = ConformanceReport {
        mvl_path: String::new(),
        execution_path: None,
        verdicts: vec![
            RuleVerdict {
                rule: ConformanceRule::Reconstruction,
                status: VerdictStatus::Pass,
                detail: "ok".into(),
            },
            RuleVerdict {
                rule: ConformanceRule::Ordering,
                status: VerdictStatus::Fail,
                detail: "broken".into(),
            },
            RuleVerdict {
                rule: ConformanceRule::JoinIntegrity,
                status: VerdictStatus::Skipped,
                detail: "n/a".into(),
            },
        ],
    };
    let failed = report.failed();
    assert_eq!(failed.len(), 1, "exactly one Fail verdict");
    assert_eq!(failed[0].rule, ConformanceRule::Ordering);
}

#[test]
fn all_checked_passed_returns_false_when_any_fail() {
    // The `all_checked_passed` body is `iter().all(|v| v.status != VerdictStatus::Fail)`.
    // cargo-mutants's `replace with true` and `!=` -> `==` mutations both make a
    // report-with-fail report as having all passed. Construct a one-Fail report and
    // assert it is *not* all-pass.
    let report = report_with(VerdictStatus::Fail);
    assert!(!report.all_checked_passed());
}

#[test]
fn all_checked_passed_returns_true_when_no_fail() {
    // Mirror of the above: a Pass+Skipped report has zero Fail verdicts, so
    // `all_checked_passed` must return true. This pins the positive direction so
    // future refactors don't accidentally flip the bool to false on the empty case.
    let report = ConformanceReport {
        mvl_path: String::new(),
        execution_path: None,
        verdicts: vec![
            RuleVerdict {
                rule: ConformanceRule::Reconstruction,
                status: VerdictStatus::Pass,
                detail: "ok".into(),
            },
            RuleVerdict {
                rule: ConformanceRule::JoinIntegrity,
                status: VerdictStatus::Skipped,
                detail: "n/a".into(),
            },
        ],
    };
    assert!(report.all_checked_passed());
}

fn jsonl_event(seq: i64, type_name: &str) -> JsonlEvent {
    JsonlEvent {
        v: 1,
        type_name: type_name.into(),
        ts: "t".into(),
        run: "r".into(),
        seq,
        body: std::collections::BTreeMap::new(),
    }
}

#[test]
fn apply_kill_prefix_is_inclusive_at_the_boundary() {
    // The body is `events.into_iter().filter(|e| e.seq <= n).collect()`. cargo-mutants's
    // `<=` -> `>` mutation makes the boundary exclusive: an event with `seq == n` is
    // dropped. A test with a known-length fixture and an exact `n == max_seq` proves
    // the inclusive bound.
    let events = vec![
        jsonl_event(0, "run_started"),
        jsonl_event(1, "prompt"),
        jsonl_event(2, "run_ended"),
    ];
    let kept = apply_kill_prefix(events.clone(), Some(1));
    assert_eq!(
        kept.len(),
        2,
        "seq<=1 must keep events at seq 0 and seq 1 (the boundary event)"
    );
    let none = apply_kill_prefix(events, None);
    assert_eq!(none.len(), 3, "no kill-after returns the full stream");
}
