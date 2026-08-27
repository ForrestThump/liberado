//! Split from `lib.rs` for module-health boundaries.

//! Tests added to close mutation survivors: every assertion here was checked to fail
//! under the specific mutant it targets (see the mutants campaign ledger).

use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    for args in [
        ["init", "--quiet"].as_slice(),
        ["config", "user.email", "test@liberado.local"].as_slice(),
        ["config", "user.name", "liberado-test"].as_slice(),
    ] {
        assert!(
            liberado_common::process::std_command(GIT)
                .args(args)
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
    }
}

// ── command_grants delegation ────────────────────────────────────────────

/// A grant allowed on the host must surface through every workspace wrapper that
/// delegates. A mutant replacing a `command_grants` body with `Default::default()`
/// silently withdraws operator-approved programs from the sandbox.
#[test]
fn granted_stems_surface_through_every_workspace_kind() {
    let dir = tempfile::tempdir().unwrap();
    let grants = CommandGrantSet::default();
    grants.allow("python");
    assert!(grants.contains("python"));

    let host = HostWorkspace::new(dir.path(), CommandPolicy::default())
        .unwrap()
        .with_command_grants(grants);
    assert!(host.command_grants().contains("python"), "host");

    let docker = DockerWorkspace {
        host: host.clone(),
        spec: DockerSandboxSpec {
            image: "test-image".into(),
            network: None,
            env_allowlist: Vec::new(),
            volumes: Vec::new(),
            user: None,
        },
    };
    assert!(
        docker.command_grants().contains("python"),
        "docker must delegate to its host grants"
    );

    let worktree = WorktreeWorkspace {
        inner: host,
        worktree_path: None,
        parent_repo: None,
    };
    assert!(
        worktree.command_grants().contains("python"),
        "worktree must delegate to its inner grants"
    );
}

// ── deny rule vs program stem ────────────────────────────────────────────

/// A deny rule containing a space is an argument-bearing *command line* rule; it must
/// never deny by bare program stem — even when a program's own file stem happens to
/// equal the rule text. (`/opt/my tool` stems to `my tool`; only a literal
/// `my tool …` command line may trip the rule.)
#[cfg(unix)]
#[test]
fn an_argument_bearing_deny_rule_never_matches_a_program_stem() {
    let policy = CommandPolicy {
        allow: Vec::new(),
        deny: vec!["my tool".to_string()],
        ..CommandPolicy::default()
    };
    let mut request = CommandRequest::new("/opt/my tool");
    request.args = vec!["--flag".to_string()];
    assert!(
        ensure_command_allowed(&policy, &request).is_ok(),
        "the '/opt/my tool' binary must not be denied by the 'my tool' command-line rule"
    );
}

// ── output decoding ──────────────────────────────────────────────────────

#[test]
fn plain_ascii_and_utf8_decode_as_utf8() {
    assert_eq!(decode_command_bytes(b"abcdefgh"), "abcdefgh");
    // Exactly four bytes: short enough that a broken high-NUL heuristic could
    // misclassify it as UTF-16LE.
    assert_eq!(decode_command_bytes(b"abcd"), "abcd");
    let utf8 = "héllo wörld";
    assert_eq!(decode_command_bytes(utf8.as_bytes()), utf8);
}

#[test]
fn bom_less_utf16le_is_detected_at_every_length_the_guard_admits() {
    assert_eq!(decode_command_bytes(b"h\0e\0y\0"), "hey");
    // Exactly four bytes: the length guard admits it, and it decodes.
    assert_eq!(decode_command_bytes(b"a\0b\0"), "ab");
    // Odd byte count cannot be UTF-16.
    assert_eq!(decode_command_bytes(b"abc"), "abc");
    // Short even buffers are rejected before the NUL heuristic: a lone "a\0" pair
    // must stay raw UTF-8, not decode as a one-unit UTF-16 string.
    assert_eq!(decode_command_bytes(b"a\0"), "a\0");
}

#[test]
fn utf16_boms_are_honoured() {
    let le: Vec<u8> = [0xFFu8, 0xFE, b'x', 0].to_vec();
    assert_eq!(decode_command_bytes(&le), "x");
    let be: Vec<u8> = [0xFEu8, 0xFF, 0, b'x'].to_vec();
    assert_eq!(decode_command_bytes(&be), "x");
}

// ── offload ids ──────────────────────────────────────────────────────────

#[test]
fn offload_ids_are_distinct_nanosecond_stamps() {
    let a = offload_id();
    let b = offload_id();
    assert!(!a.is_empty(), "an empty id collides every offload file");
    assert!(a.parse::<u128>().is_ok(), "ids are decimal stamps, got {a}");
    assert!(b.parse::<u128>().is_ok(), "ids are decimal stamps, got {b}");
    assert_ne!(a, b, "two calls must not mint the same suffix");
}

// ── truncation helpers ───────────────────────────────────────────────────

#[test]
fn truncate_head_respects_limits_and_boundaries() {
    assert_eq!(truncate_head("hello", 3), "hel");
    assert_eq!(truncate_head("hello", 99), "hello");
    assert_eq!(truncate_head("hello", 0), "");
    // "é" is two bytes: a cut at 2 lands inside the character and must back up to 1.
    assert_eq!(truncate_head("aébc", 2), "a");
    // Halving or inverting the walk must not stop at an earlier boundary.
    assert_eq!(truncate_head("ééé", 5), "éé");
}

#[test]
fn char_boundary_search_walks_back_to_a_real_boundary() {
    let s = "aébc";
    assert_eq!(char_boundary_at_or_before(s, 5), s.len());
    assert_eq!(char_boundary_at_or_before(s, 2), 1);
    assert_eq!(char_boundary_at_or_before(s, 1), 1);
    assert_eq!(char_boundary_at_or_before(s, 0), 0);
    // Multi-byte run: from index 5 the walk must reach 4, not 2 or overshoot.
    assert_eq!(char_boundary_at_or_before("ééé", 5), 4);
}

#[test]
fn head_tail_preview_keeps_both_ends_with_the_marker_between() {
    let text = format!("HEAD{}TAIL", "x".repeat(200));
    let preview = head_tail_preview(&text, 40);
    assert!(preview.starts_with("HEAD"), "{preview:?}");
    assert!(preview.ends_with("TAIL"), "{preview:?}");
    assert!(preview.contains("middle omitted"), "{preview:?}");

    // Exact split: head gets ceil? No — head = max/2, tail = max - head, and the
    // tail window starts exactly `tail` bytes from the end. Pin the whole string so
    // any drift in the head/tail arithmetic changes this assertion.
    let text2 = "0123456789abcdefghij"; // 20 bytes
    let expected = format!(
        "{}\n\n\u{2026} [output truncated to 10 bytes of 20; middle omitted] \u{2026}\n\n{}",
        &text2[..5],
        &text2[15..]
    );
    assert_eq!(head_tail_preview(text2, 10), expected);
}

// ── durable session worktrees ────────────────────────────────────────────

#[tokio::test]
async fn session_worktree_is_created_reused_and_self_heals() {
    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().join("parent");
    init_repo(&parent);
    std::fs::write(parent.join("seed.txt"), "seed").unwrap();
    for args in [
        ["add", "."].as_slice(),
        ["commit", "--quiet", "-m", "seed"].as_slice(),
    ] {
        assert!(
            liberado_common::process::std_command(GIT)
                .args(args)
                .current_dir(&parent)
                .status()
                .unwrap()
                .success()
        );
    }
    let base = temp.path().join("worktrees");

    let first = ensure_session_worktree(&parent, "sess1", &base)
        .await
        .unwrap();
    assert!(
        first.join(".git").exists(),
        "linked worktree has git metadata"
    );
    assert_eq!(first.file_name().unwrap(), "sess1");

    // Reuse: mid-build park/resume lands on the same files, marker included.
    std::fs::write(first.join("marker.txt"), "parked").unwrap();
    let second = ensure_session_worktree(&parent, "sess1", &base)
        .await
        .unwrap();
    assert_eq!(
        second.canonicalize().unwrap(),
        first.canonicalize().unwrap(),
        "reuse must return the same worktree"
    );
    assert_eq!(
        std::fs::read_to_string(second.join("marker.txt")).unwrap(),
        "parked",
        "reuse must keep attempt-local files"
    );

    // A directory without `.git` is a broken leftover: recreate, don't reuse.
    std::fs::remove_file(second.join(".git")).unwrap();
    let third = ensure_session_worktree(&parent, "sess1", &base)
        .await
        .unwrap();
    assert!(
        third.join(".git").exists(),
        "broken leftover must be recreated with fresh git metadata"
    );

    // Unsafe ids are refused before any path is touched.
    assert!(
        ensure_session_worktree(&parent, "bad/id", &base)
            .await
            .is_err()
    );
}

// ── best-effort git is observable ────────────────────────────────────────

#[tokio::test]
async fn best_effort_git_returns_the_underlying_outcome() {
    let dir = std::env::temp_dir().join(format!("sb-be-{}", unique()));
    init_repo(&dir);
    let out = run_git_best_effort(&dir, &["status", "--porcelain"]).await;
    assert!(out.is_ok(), "clean status should succeed: {out:?}");
    let err = run_git_best_effort(&dir, &["this-is-not-a-git-subcommand"]).await;
    assert!(err.is_err(), "garbage subcommands surface their failure");
    let _ = std::fs::remove_dir_all(&dir);
}
