//! Conformance fixtures for the MVL + execution-log contracts (backlog 0.5).
//!
//! Loads real fixture files from disk (so a missing artifact fails the suite), then drives
//! the shipped reconstruction and join functions in `liberado_test_support::trace_contracts`.

use std::path::PathBuf;

use liberado_test_support::trace_contracts::{
    assert_attempt_brackets, assert_join_integrity, assert_mvl_has_no_scheduler_leakage,
    assert_seq_gap_free, parse_jsonl, reconstruct_turn,
};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/trace_contracts")
}

fn load(name: &str) -> String {
    let path = fixtures_dir().join(name);
    assert!(
        path.is_file(),
        "conformance fixture missing: {} — backlog 0.5 requires on-disk samples",
        path.display()
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn fixture_files_exist() {
    for name in ["sample.mvl.jsonl", "sample.execution.jsonl"] {
        let p = fixtures_dir().join(name);
        assert!(p.is_file(), "missing {}", p.display());
        let bytes = std::fs::metadata(&p).unwrap().len();
        assert!(
            bytes > 200,
            "{} too small to be a real fixture",
            p.display()
        );
    }
}

#[test]
fn mvl_fixture_reconstructs_every_turn() {
    let mvl = parse_jsonl(&load("sample.mvl.jsonl")).expect("parse mvl");
    assert_seq_gap_free(&mvl).expect("mvl seq");
    assert_mvl_has_no_scheduler_leakage(&mvl).expect("no scheduler leak");

    // Turn 0: full messages only.
    let t0 = reconstruct_turn(&mvl, 0).expect("turn 0");
    assert_eq!(
        t0.system_text,
        "You are Liberado's coding agent. Prefer real tests."
    );
    assert_eq!(t0.tools_offered, vec!["read_file", "edit_file"]);
    assert_eq!(t0.messages.len(), 1);
    assert_eq!(
        t0.params.get("temperature").and_then(|v| v.as_f64()),
        Some(0.1)
    );
    assert_eq!(
        t0.params.get("max_tokens").and_then(|v| v.as_i64()),
        Some(4096)
    );
    let tools = t0.tool_definitions.as_array().expect("tools array");
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["name"], "read_file");
    assert_eq!(tools[1]["name"], "edit_file");

    // Turn 1: delta appended; system recovered by hash without re-emitting text.
    let t1 = reconstruct_turn(&mvl, 1).expect("turn 1");
    assert_eq!(t1.system_text, t0.system_text);
    assert_eq!(t1.system_sha256, t0.system_sha256);
    assert!(t1.messages.len() > t0.messages.len());
    assert_eq!(t1.tool_catalog_sha256, t0.tool_catalog_sha256);
    assert_eq!(t1.tool_definitions, t0.tool_definitions);

    // Turn 2: tools_offered narrowed after tools_changed (guard withdrew edit_file).
    let t2 = reconstruct_turn(&mvl, 2).expect("turn 2");
    assert_eq!(t2.tools_offered, vec!["read_file"]);
    assert_eq!(t2.system_text, t0.system_text);
    assert!(
        t2.messages.len() >= t1.messages.len(),
        "deltas accumulate until a full reset"
    );
}

#[test]
fn execution_fixture_joins_mvl_without_timestamps() {
    let mvl = parse_jsonl(&load("sample.mvl.jsonl")).expect("parse mvl");
    let ex = parse_jsonl(&load("sample.execution.jsonl")).expect("parse exec");
    assert_seq_gap_free(&ex).expect("exec seq");
    assert_attempt_brackets(&ex).expect("attempts");
    assert_join_integrity(&mvl, &ex).expect("join by call_id");

    // Structural: every execution tool_finished has duration_ms (scheduler fact).
    let finished: Vec<_> = ex
        .iter()
        .filter(|e| e.type_name == "tool_finished")
        .collect();
    assert_eq!(finished.len(), 2);
    for f in finished {
        assert!(
            f.body.get("duration_ms").and_then(|v| v.as_i64()).is_some(),
            "tool_finished must carry duration_ms in the execution log"
        );
    }
}

#[test]
fn specs_are_present_in_the_repo() {
    // Contract docs are part of the deliverable; a deleted path must fail CI.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("docs/spec/reference");
    for name in ["model-view-log.md", "execution-log.md"] {
        let p = root.join(name);
        assert!(p.is_file(), "missing contract {}", p.display());
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(
            text.contains("Conformance") || text.contains("conformance"),
            "{} must document conformance",
            p.display()
        );
    }
    let mvl = std::fs::read_to_string(root.join("model-view-log.md")).unwrap();
    assert!(
        mvl.contains("Reconstruction checklist"),
        "MVL must carry the reconstruction checklist used by fixtures"
    );
    let ex = std::fs::read_to_string(root.join("execution-log.md")).unwrap();
    assert!(
        ex.contains("Join integrity") || ex.contains("join"),
        "execution log must require join integrity"
    );
}
