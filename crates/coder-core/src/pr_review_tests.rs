//! Split from `pr_review.rs` for module-health boundaries.

use super::*;
use crate::ReviewWorkerConfig;

fn valid(sha: &str) -> String {
    serde_json::json!({"schema":REVIEW_SCHEMA_VERSION,"reviewed_sha":sha,"summary":"clean","findings":[]}).to_string()
}

#[test]
fn success_requires_exact_full_sha_and_valid_schema() {
    let sha = "a".repeat(40);
    assert!(ReviewResult::parse_success(&valid(&sha), &sha).is_ok());
    assert_eq!(
        ReviewResult::parse_success(&valid(&"b".repeat(40)), &sha),
        Err(ReviewResultError::StaleSha)
    );
    assert_eq!(
        ReviewResult::parse_success("{}", &sha),
        Err(ReviewResultError::Malformed)
    );
    assert_eq!(
        ReviewResult::parse_success(&valid("abc"), "abc"),
        Err(ReviewResultError::StaleSha)
    );
}

#[test]
fn output_schema_types_constant_and_enum_properties() {
    let schema: serde_json::Value =
        serde_json::from_str(crate::pr_review_port::REVIEW_RESULT_SCHEMA).expect("valid schema");
    assert_eq!(
        schema.pointer("/properties/schema/type"),
        Some(&"string".into())
    );
    assert_eq!(
        schema.pointer("/properties/findings/items/properties/severity/type"),
        Some(&"string".into())
    );
}

#[test]
fn extracts_opencode_json_event_text() {
    let sha = "a".repeat(40);
    let result = valid(&sha);
    let output = format!(
        "{}\n",
        serde_json::json!({"type":"text","part":{"text": result}})
    );
    let parsed = parse_opencode_success(&output, &sha).expect("opencode event");
    assert_eq!(parsed.summary, "clean");
}

#[test]
fn extracts_codex_jsonl_result() {
    let output = include_str!("../tests/fixtures/codex_review_jsonl_success.jsonl");
    let result = parse_codex_success(output, &"a".repeat(40)).unwrap();
    assert_eq!(result.summary, "No blocking issues.");
}

#[test]
fn codex_failure_fixtures_are_distinct_and_402_is_not_exhaustion() {
    assert_eq!(
        classify_codex_failure("ERROR: You've hit your usage limit. Try again at 1:00 PM"),
        WorkerFailure::Exhausted
    );
    assert_eq!(
        classify_codex_failure("■ You've hit your usage limit. Try again at 9:45 PM."),
        WorkerFailure::Exhausted
    );
    assert_eq!(
        classify_codex_failure("You've reached your weekly limit"),
        WorkerFailure::Exhausted
    );
    assert_eq!(
        classify_codex_failure("request rejected: usage limit reached"),
        WorkerFailure::Exhausted
    );
    assert_eq!(
        classify_codex_failure("HTTP 429: rate limit"),
        WorkerFailure::RateLimited
    );
    assert_eq!(
        classify_codex_failure("Unauthorized: authentication failed"),
        WorkerFailure::Auth
    );
    assert_eq!(
        classify_codex_failure("Permission denied by sandbox"),
        WorkerFailure::Permission
    );
    assert_eq!(
        classify_codex_failure("operation timed out"),
        WorkerFailure::Timeout
    );
    assert_eq!(
        classify_codex_failure("HTTP 402 Payment Required"),
        WorkerFailure::ModelFailure
    );
    assert_eq!(
        classify_codex_failure("model returned an error"),
        WorkerFailure::ModelFailure
    );
}

#[test]
fn codex_command_is_read_only_and_sha_pinned() {
    let args = codex_review_args("base", "head", "schema.json");
    assert_eq!(
        args,
        [
            "exec",
            "review",
            "--sandbox",
            "read-only",
            "--json",
            "--output-schema",
            "schema.json",
            "--base",
            "base",
            "--commit",
            "head"
        ]
    );
}

#[test]
fn opencode_command_is_json_run_without_auto_approve() {
    let args = opencode_review_args(
        OPENCODE_NAMED_REVIEW_MODEL,
        "schema.json",
        &review_prompt("base", "head"),
    );
    assert_eq!(args[0], "run");
    assert!(args.contains(&"--format".into()));
    assert!(args.contains(&"json".into()));
    assert!(args.contains(&OPENCODE_NAMED_REVIEW_MODEL.into()));
    assert!(!args.iter().any(|arg| arg == "--auto"));
}

#[test]
fn adapter_commands_are_frozen_without_execution() {
    assert_eq!(
        grok_review_args("schema"),
        ["review", "--headless", "--schema", "schema"]
    );
    assert_eq!(
        antigravity_review_args("schema"),
        [
            "--print",
            "--output-format",
            "stream-json",
            "--schema",
            "schema"
        ]
    );
    assert_eq!(cursor_review_args(), ["--mode", "ask", "--print"]);
}

fn policy() -> ReviewPolicy {
    ReviewPolicy {
        controller: "liberado-shepherd".into(),
        check_names: vec!["CI".into()],
        shadow: false,
    }
}
fn pr(repo: &str, sha: &str) -> PullRequestSnapshot {
    PullRequestSnapshot {
        repository: repo.into(),
        number: 1,
        head_sha: sha.into(),
        open: true,
        draft: false,
        fork_head: false,
    }
}
fn checks(sha: &str, conclusion: CheckConclusion) -> ShaChecks {
    ShaChecks {
        sha: sha.into(),
        checks: vec![("CI".into(), conclusion)],
    }
}

#[test]
fn eligibility_is_sha_exact_success_only_and_rejects_forks() {
    let a = "a".repeat(40);
    let b = "b".repeat(40);
    let cycle = ReviewCycle {
        armed_sha: Some(a.clone()),
        accepted_sha: None,
        accepted_review_key: None,
    };
    assert!(review_eligible(
        &policy(),
        &pr("one/repo", &a),
        &cycle,
        &checks(&a, CheckConclusion::Success)
    ));
    assert!(!review_eligible(
        &policy(),
        &pr("one/repo", &a),
        &cycle,
        &checks(&b, CheckConclusion::Success)
    ));
    assert!(!review_eligible(
        &policy(),
        &pr("one/repo", &a),
        &cycle,
        &checks(&a, CheckConclusion::Pending)
    ));
    let mut fork = pr("one/repo", &a);
    fork.fork_head = true;
    assert!(!review_eligible(
        &policy(),
        &fork,
        &cycle,
        &checks(&a, CheckConclusion::Success)
    ));
}

#[test]
fn wake_rules_do_not_infer_an_arm() {
    let sha = "a".repeat(40);
    let empty = ReviewCycle {
        armed_sha: None,
        accepted_sha: None,
        accepted_review_key: None,
    };
    for signal in [
        WakeSignal::Poll,
        WakeSignal::ReviewRequested,
        WakeSignal::Opened,
    ] {
        assert!(
            !observe(
                &policy(),
                &pr("one/repo", &sha),
                &empty,
                &checks(&sha, CheckConclusion::Success),
                signal
            )
            .iter()
            .any(|intent| matches!(
                intent,
                ObserverIntent::Arm { .. } | ObserverIntent::Eligible { .. }
            ))
        );
    }
}

#[test]
fn synchronize_is_one_draft_note_intent_and_never_review() {
    let old = "a".repeat(40);
    let new = "b".repeat(40);
    let cycle = ReviewCycle {
        armed_sha: Some(old.clone()),
        accepted_sha: None,
        accepted_review_key: None,
    };
    let intents = observe(
        &policy(),
        &pr("one/repo", &new),
        &cycle,
        &checks(&new, CheckConclusion::Success),
        WakeSignal::Synchronize { old_sha: old },
    );
    assert_eq!(
        intents
            .iter()
            .filter(|i| matches!(i, ObserverIntent::SynchronizeDraftAndNote { .. }))
            .count(),
        1
    );
    assert!(
        !intents
            .iter()
            .any(|i| matches!(i, ObserverIntent::Eligible { .. }))
    );
}

#[test]
fn repository_identity_and_shadow_lease_are_explicit() {
    assert_ne!(
        pr("one/repo", &"a".repeat(40)),
        pr("two/repo", &"a".repeat(40))
    );
    let mut p = policy();
    p.shadow = true;
    assert!(!controller_lease_required(&p));
}

#[test]
fn api_paths_are_sha_exact_and_page_iteration_is_bounded() {
    let paths = check_endpoints("owner/repo", "abc");
    assert_eq!(
        paths[0],
        "/repos/owner/repo/commits/abc/check-runs?filter=latest&per_page=100"
    );
    assert_eq!(
        paths[1],
        "/repos/owner/repo/commits/abc/status?per_page=100"
    );
    assert_eq!(bounded_pages(3).collect::<Vec<_>>(), [1, 2, 3]);
    let identity = format!("{}Thump/liberado", "Forrest");
    assert!(
        !include_str!("pr_review.rs").contains(&identity),
        "review contract must not hardcode a repository identity"
    );
}

#[test]
fn accepted_sha_and_non_success_conclusions_cannot_pass() {
    let a = "a".repeat(40);
    let cycle = ReviewCycle {
        armed_sha: Some(a.clone()),
        accepted_sha: Some(a.clone()),
        accepted_review_key: Some(review_key("one/repo", 1, &a, POLICY_VERSION)),
    };
    assert!(!review_eligible(
        &policy(),
        &pr("one/repo", &a),
        &cycle,
        &checks(&a, CheckConclusion::Success)
    ));
    for conclusion in [CheckConclusion::Pending, CheckConclusion::Failure] {
        let armed = ReviewCycle {
            armed_sha: Some(a.clone()),
            accepted_sha: None,
            accepted_review_key: None,
        };
        assert!(!review_eligible(
            &policy(),
            &pr("one/repo", &a),
            &armed,
            &checks(&a, conclusion)
        ));
    }
    for raw in [
        "neutral",
        "skipped",
        "stale",
        "cancelled",
        "timed_out",
        "action_required",
        "startup_failure",
    ] {
        assert_eq!(
            parse_check_conclusion(Some(raw), None),
            CheckConclusion::Failure
        );
    }
    assert_eq!(
        parse_check_conclusion(None, Some("in_progress")),
        CheckConclusion::Pending
    );
}

#[test]
fn poll_ready_open_does_not_arm_and_review_request_never_arrives() {
    let sha = "a".repeat(40);
    let empty = ReviewCycle {
        armed_sha: None,
        accepted_sha: None,
        accepted_review_key: None,
    };
    let intents = reconcile_snapshot(
        &policy(),
        &pr("one/repo", &sha),
        &empty,
        &checks(&sha, CheckConclusion::Success),
        None,
        false,
    );
    assert!(!intents.iter().any(|intent| matches!(
        intent,
        ObserverIntent::Arm { .. } | ObserverIntent::Eligible { .. }
    )));
    assert_eq!(
        poll_signal(&pr("one/repo", &sha), &empty, None, false),
        WakeSignal::Poll
    );
    assert_eq!(
        ready_for_review_commit(&serde_json::json!({"event":"review_requested","commit_id":sha})),
        None
    );
    assert_eq!(
        ready_for_review_commit(&serde_json::json!({"event":"opened","commit_id":sha})),
        None
    );
    assert_eq!(
        ready_for_review_commit(&serde_json::json!({"event":"ready_for_review","commit_id":sha})),
        Some(sha.as_str())
    );
}

#[test]
fn restart_rebuilds_cycle_and_slice01_does_not_dispatch() {
    let sha = "a".repeat(40);
    let next = "b".repeat(40);
    let cycle = review_cycle([
        CycleFact::Armed { sha: sha.clone() },
        CycleFact::Synchronized,
    ]);
    assert_eq!(cycle.armed_sha, None);
    let restored = review_cycle([CycleFact::Armed { sha: sha.clone() }]);
    let again = reconcile_snapshot(
        &policy(),
        &pr("one/repo", &sha),
        &restored,
        &checks(&sha, CheckConclusion::Success),
        Some(sha.as_str()),
        false,
    );
    assert_eq!(
        again
            .iter()
            .filter(|intent| matches!(intent, ObserverIntent::Arm { .. }))
            .count(),
        0
    );
    assert!(
        again
            .iter()
            .any(|intent| matches!(intent, ObserverIntent::Eligible { .. }))
    );
    assert!(again.iter().any(slice01_records));
    assert!(!slice01_records(&ObserverIntent::Eligible {
        sha: next.clone()
    }));
    let sync = reconcile_snapshot(
        &policy(),
        &pr("one/repo", &next),
        &restored,
        &checks(&next, CheckConclusion::Success),
        Some(sha.as_str()),
        true,
    );
    assert_eq!(
        sync.iter()
            .filter(|intent| matches!(intent, ObserverIntent::SynchronizeDraftAndNote { .. }))
            .count(),
        1
    );
    assert!(
        !sync
            .iter()
            .any(|intent| matches!(intent, ObserverIntent::Eligible { .. }))
    );
}

#[test]
fn github_payloads_and_disabled_grok_start_no_process() {
    let sha = "a".repeat(40);
    let snapshot = pull_request_snapshot(
        "one/repo",
        &serde_json::json!({
            "number": 7,
            "state": "open",
            "draft": false,
            "head": {"sha": sha, "repo": {"full_name": "fork/repo"}},
            "base": {"repo": {"full_name": "one/repo"}}
        }),
    )
    .unwrap();
    assert!(snapshot.fork_head);
    let checks = collect_github_checks(&serde_json::json!({
        "check_runs": [
            {"name": "CI", "conclusion": "success"},
            {"name": "skip", "conclusion": "skipped"}
        ]
    }));
    assert_eq!(
        checks,
        [
            ("CI".into(), CheckConclusion::Success),
            ("skip".into(), CheckConclusion::Failure)
        ]
    );
    let grok = ReviewWorkerConfig::GrokBuild {
        executable: "/must-not-run/grok".into(),
        enabled: false,
    };
    assert_eq!(review_process_argv(&grok, "schema.json"), None);
    let mut started = false;
    if let Some(argv) = review_process_argv(&grok, "schema.json") {
        started = true;
        let _ = std::process::Command::new(&argv[0]);
    }
    assert!(!started);
}

#[test]
fn review_process_argv_covers_each_enabled_adapter_and_skips_http() {
    let schema = "schema.json";
    let grok = review_process_argv(
        &ReviewWorkerConfig::GrokBuild {
            executable: "grok".into(),
            enabled: true,
        },
        schema,
    )
    .unwrap();
    assert_eq!(grok[0], "grok");
    assert!(grok.contains(&"--headless".into()));

    let codex = review_process_argv(
        &ReviewWorkerConfig::Codex {
            executable: "codex".into(),
            enabled: true,
        },
        schema,
    )
    .unwrap();
    assert_eq!(codex[0], "codex");
    assert!(codex.contains(&"--sandbox".into()));

    let antigravity = review_process_argv(
        &ReviewWorkerConfig::Antigravity {
            executable: "agy".into(),
            enabled: true,
            print_timeout: "30s".into(),
        },
        schema,
    )
    .unwrap();
    assert_eq!(antigravity[0], "agy");
    assert!(antigravity.contains(&"stream-json".into()));

    let cursor = review_process_argv(
        &ReviewWorkerConfig::CursorLocal {
            executable: "cursor".into(),
            enabled: true,
        },
        schema,
    )
    .unwrap();
    assert_eq!(cursor[0], "cursor");
    assert!(cursor.contains(&"--mode".into()));

    let open_code = review_process_argv(
        &ReviewWorkerConfig::OpenCode {
            executable: "opencode".into(),
            model: OPENCODE_NAMED_REVIEW_MODEL.into(),
            permission_mode: "deny_writes".into(),
            pricing_policy: "named".into(),
            enabled: true,
        },
        schema,
    )
    .unwrap();
    assert_eq!(open_code[0], "opencode");
    assert!(open_code.contains(&"run".into()));
    assert!(!open_code.iter().any(|arg| arg == "--auto"));

    assert_eq!(
        review_process_argv(
            &ReviewWorkerConfig::OpenaiCompatible {
                base_url: "http://127.0.0.1:9/v1".into(),
                model: "auto".into(),
                pricing_policy: "zero_only".into(),
                enabled: true,
            },
            schema,
        ),
        None
    );
}

#[test]
fn jsonl_fixture_extracts_final_agent_message() {
    let fixture = include_str!("../tests/fixtures/codex_review_jsonl_success.jsonl");
    let sha = "a".repeat(40);
    let result = parse_codex_success(fixture, &sha).expect("jsonl fixture");
    assert_eq!(result.reviewed_sha, sha);
    assert!(result.findings.is_empty());
}

#[test]
fn jsonl_rejects_conflicting_agent_messages() {
    let sha = "a".repeat(40);
    let one = format!(
        "{{\"schema\":\"{}\",\"reviewed_sha\":\"{sha}\",\"summary\":\"one\",\"findings\":[]}}",
        REVIEW_SCHEMA_VERSION
    );
    let two = format!(
        "{{\"schema\":\"{}\",\"reviewed_sha\":\"{sha}\",\"summary\":\"two\",\"findings\":[]}}",
        REVIEW_SCHEMA_VERSION
    );
    let line = |text: &str| {
        serde_json::json!({
            "type": "item.completed",
            "item": {"id": "1", "type": "agent_message", "text": text}
        })
        .to_string()
    };
    let output = format!(
        "{}
{}
",
        line(&one),
        line(&two)
    );
    assert!(matches!(
        parse_codex_success(&output, &sha),
        Err(ReviewResultError::Malformed)
    ));
}

#[test]
fn jsonl_rejects_duplicate_and_trailing_invalid_agent_messages() {
    let sha = "a".repeat(40);
    let result = format!(
        "{{\"schema\":\"{}\",\"reviewed_sha\":\"{sha}\",\"summary\":\"one\",\"findings\":[]}}",
        REVIEW_SCHEMA_VERSION
    );
    let line = |text: &str| {
        serde_json::json!({
            "type": "item.completed",
            "item": {"id": "1", "type": "agent_message", "text": text}
        })
        .to_string()
    };

    let duplicate = format!("{}\n{}\n", line(&result), line(&result));
    assert!(matches!(
        parse_codex_success(&duplicate, &sha),
        Err(ReviewResultError::Malformed)
    ));

    let trailing_invalid = format!("{}\n{}\n", line(&result), line("not review JSON"));
    assert!(matches!(
        parse_codex_success(&trailing_invalid, &sha),
        Err(ReviewResultError::Malformed)
    ));
}
