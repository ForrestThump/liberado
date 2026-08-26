//! Ledger-persistence survivor and contract tests: locked concurrent
//! append, dedupe-on-save, and clean failure when the rename cannot land.
//! Split from `mutants_cmd_tests.rs` for the module-health boundary.

use super::super::{Campaign, Counts, LEDGER_FILE, Ledger};
use super::*;
use std::fs;

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
fn save_ledger_fails_cleanly_when_the_target_cannot_be_renamed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(LEDGER_FILE)).unwrap();
    let ledger = Ledger {
        schema: 1,
        campaigns: vec![],
    };

    let outcome = save_ledger(dir.path(), &ledger);

    assert!(
        outcome.is_err(),
        "renaming onto a directory must fail: {outcome:?}"
    );
    let litter: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(
        litter.is_empty(),
        "no inert temp files may remain: {litter:?}"
    );
}

/// read-modify-write, so without the ledger lock the last rename would
/// silently drop whichever row it did not carry.

#[test]
fn concurrent_appends_both_survive_the_read_modify_write() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join(LEDGER_FILE),
        r#"{"schema":1,"campaigns":[{"package":"liberado-seed","commit":null,"recorded_at":"2026-08-25","scope":"package","source":"markdown-seed","counts":{"viable":1,"caught":1,"survived":0,"timeout":0,"unviable":0}}]}"#,
    )
    .unwrap();

    fn row(package: &str) -> Campaign {
        Campaign {
            package: package.into(),
            commit: Some("feedface".into()),
            recorded_at: "2026-08-25".into(),
            command: Some("cargo mutants".into()),
            tool_version: Some("27.1.0".into()),
            scope: "package".into(),
            counts: Counts {
                viable: 2,
                caught: 2,
                survived: 0,
                timeout: 0,
                unviable: 0,
            },
            source: None,
        }
    }

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut handles = Vec::new();
    for package in ["liberado-agent-a", "liberado-agent-b"] {
        let root = root.to_path_buf();
        let barrier = std::sync::Arc::clone(&barrier);
        // `Box<dyn Error>` is not `Send`; flatten to a string inside the thread.
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            append_campaign(&root, row(package)).map_err(|e| e.to_string())
        }));
    }
    for handle in handles {
        handle.join().unwrap().unwrap();
    }

    let packages: Vec<String> = load_ledger(root)
        .unwrap()
        .campaigns
        .iter()
        .map(|c| c.package.clone())
        .collect();
    assert!(
        packages.contains(&"liberado-agent-a".to_string())
            && packages.contains(&"liberado-agent-b".to_string()),
        "both concurrent rows must survive: {packages:?}"
    );
    assert_eq!(packages.len(), 3, "seed plus exactly two appends");
}
