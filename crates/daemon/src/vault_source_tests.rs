use super::*;

/// Empty capture paths and empty flags: only `proposals/` is in scope. Used when a test
/// exercises ignore globs on a path that is not under `inbox/` — ignore runs first, so the
/// scope never sees those paths when the glob matches.
fn unscoped() -> CaptureScope {
    CaptureScope::new(Vec::<String>::new(), "", "")
}

/// The production-default scope: `inbox/` capture path, `#ready-now` ready flag,
/// `#hold-off` hold flag.
fn default_scope() -> CaptureScope {
    CaptureScope::production_default()
}

/// Build a real on-disk vault in a temp dir, with one test file under `inbox/` (the default
/// capture path) written directly (no audit entry, so `attribute()` returns `External`).
async fn test_vault() -> (tempfile::TempDir, Vault, std::path::PathBuf) {
    vault_with_note("inbox/test-note.md", "# hello").await
}

async fn vault_with_note(
    rel: &str,
    content: &str,
) -> (tempfile::TempDir, Vault, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = Vault::open("test", dir.path()).await.expect("open vault");
    let note_rel = std::path::PathBuf::from(rel);
    let note_abs = dir.path().join(&note_rel);
    if let Some(parent) = note_abs.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&note_abs, content).expect("write note");
    (dir, vault, note_rel)
}

#[tokio::test]
async fn matching_glob_is_dropped() {
    let (_dir, vault, rel) = test_vault().await;
    let ignore_globs = vec!["*.md".to_string()];
    let scope = default_scope();
    let result = attribute_and_build_event(&vault, &rel, &ignore_globs, &scope)
        .await
        .expect("should not error");
    assert!(result.is_none(), "path matching glob must be dropped");
}

#[tokio::test]
async fn non_matching_glob_still_produces_event() {
    let (_dir, vault, rel) = test_vault().await;
    let ignore_globs = vec!["*.txt".to_string()];
    let scope = default_scope();
    let result = attribute_and_build_event(&vault, &rel, &ignore_globs, &scope)
        .await
        .expect("should not error");
    assert!(
        result.is_some(),
        "non-matching path must still produce an event"
    );
}

#[tokio::test]
async fn empty_glob_list_changes_nothing() {
    let (_dir, vault, rel) = test_vault().await;
    let scope = default_scope();
    let result_no_globs = attribute_and_build_event(&vault, &rel, &[], &scope)
        .await
        .expect("should not error");
    let result_empty_vec = attribute_and_build_event(&vault, &rel, &Vec::new(), &scope)
        .await
        .expect("should not error");
    assert!(result_no_globs.is_some(), "no globs → event");
    assert!(result_empty_vec.is_some(), "empty vec → event");
}

#[tokio::test]
async fn agent_attributed_writes_are_still_dropped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = Vault::open("test", dir.path()).await.expect("open vault");
    let rel = std::path::PathBuf::from("inbox/agent-note.md");
    let abs = dir.path().join(&rel);
    std::fs::create_dir_all(abs.parent().unwrap()).ok();
    // Write through the vault with Agent provenance so attribution sees it as Agent.
    let prov = liberado_common::WriteProvenance::agent("test-agent", "c1");
    vault
        .write(&rel, "# agent wrote this", None, &prov)
        .await
        .expect("vault write");
    // Even with empty ignore globs, agent-attributed writes must still be dropped.
    let scope = default_scope();
    let result = attribute_and_build_event(&vault, &rel, &[], &scope)
        .await
        .expect("should not error");
    assert!(
        result.is_none(),
        "agent-attributed writes must be dropped regardless of globs"
    );
}

#[tokio::test]
async fn glob_on_filename_matches() {
    let (dir, vault, _rel) = test_vault().await;
    // Create a file matching a basename glob (like `~*` for temp files).
    let rel = std::path::PathBuf::from("~tempfile.md");
    let abs = dir.path().join(&rel);
    std::fs::write(&abs, "# temp").expect("write temp");
    let ignore_globs = vec!["~*".to_string()];
    let scope = unscoped();
    let result = attribute_and_build_event(&vault, &rel, &ignore_globs, &scope)
        .await
        .expect("should not error");
    assert!(
        result.is_none(),
        "basename glob `~*` must match ~tempfile.md"
    );
}

#[tokio::test]
async fn glob_on_directory_matches() {
    let (dir, vault, _rel) = test_vault().await;
    let rel = std::path::PathBuf::from(".stversions/some-file.md");
    let abs = dir.path().join(&rel);
    std::fs::create_dir_all(abs.parent().unwrap()).ok();
    std::fs::write(&abs, "# versioned").expect("write versioned");
    let ignore_globs = vec![".stversions/".to_string()];
    let scope = unscoped();
    let result = attribute_and_build_event(&vault, &rel, &ignore_globs, &scope)
        .await
        .expect("should not error");
    assert!(
        result.is_none(),
        "directory glob .stversions/ must match paths under .stversions/"
    );
}

// ── Positive scope tests (F12) ──────────────────────────────────────────

#[tokio::test]
async fn note_in_capture_path_produces_event() {
    let (_dir, vault, rel) = vault_with_note("inbox/capture-me.md", "a quick jot").await;
    let result = attribute_and_build_event(&vault, &rel, &[], &default_scope())
        .await
        .expect("should not error");
    assert!(
        result.is_some(),
        "note under inbox/ must be in watcher scope"
    );
}

#[tokio::test]
async fn note_with_ready_flag_anywhere_produces_event() {
    let (_dir, vault, rel) =
        vault_with_note("knowledge/urgent-thought.md", "this is urgent #ready-now").await;
    let result = attribute_and_build_event(&vault, &rel, &[], &default_scope())
        .await
        .expect("should not error");
    assert!(
        result.is_some(),
        "note with #ready-now anywhere must be in watcher scope"
    );
}

#[tokio::test]
async fn unflagged_note_outside_capture_path_is_dropped() {
    let (_dir, vault, rel) =
        vault_with_note("knowledge/random-note.md", "a random thought, no flags").await;
    let result = attribute_and_build_event(&vault, &rel, &[], &default_scope())
        .await
        .expect("should not error");
    assert!(
        result.is_none(),
        "unflagged note outside inbox/ must be dropped (belongs to schedule)"
    );
}

#[tokio::test]
async fn hold_flag_parks_note_even_in_capture_path() {
    let (_dir, vault, rel) =
        vault_with_note("inbox/deferred.md", "this is deferred #hold-off").await;
    let result = attribute_and_build_event(&vault, &rel, &[], &default_scope())
        .await
        .expect("should not error");
    assert!(
        result.is_none(),
        "#hold-off in a note must drop it even if inside inbox/"
    );
}

#[tokio::test]
async fn hold_flag_trumps_ready_flag() {
    let (_dir, vault, rel) =
        vault_with_note("anywhere/both-flags.md", "#ready-now but #hold-off here").await;
    let result = attribute_and_build_event(&vault, &rel, &[], &default_scope())
        .await
        .expect("should not error");
    assert!(result.is_none(), "#hold-off must trump #ready-now");
}

#[tokio::test]
async fn note_under_active_proposals_dir_is_always_in_scope() {
    let (_dir, vault, rel) = vault_with_note(
        "proposals/approve-me.md",
        "---\nstatus: approved\n---\ngo ahead",
    )
    .await;
    let result = attribute_and_build_event(&vault, &rel, &[], &default_scope())
        .await
        .expect("should not error");
    assert!(
        result.is_some(),
        "active proposals/ note must always be in watcher scope"
    );
}

#[tokio::test]
async fn note_under_proposals_archive_is_dropped() {
    let (_dir, vault, rel) = vault_with_note(
        "proposals/archive/approved/old.md",
        "---\nstatus: done\n---\narchived",
    )
    .await;
    let result = attribute_and_build_event(&vault, &rel, &[], &default_scope())
        .await
        .expect("should not error");
    assert!(
        result.is_none(),
        "archived proposals/ must never re-enter the pipeline"
    );
}

#[tokio::test]
async fn note_in_capture_subfolder_produces_event() {
    let (_dir, vault, rel) = vault_with_note("inbox/sub/deep-thought.md", "deep").await;
    let result = attribute_and_build_event(&vault, &rel, &[], &default_scope())
        .await
        .expect("should not error");
    assert!(
        result.is_some(),
        "note in inbox/sub/ must be in watcher scope"
    );
}

#[tokio::test]
async fn sibling_prefix_is_not_in_capture_path() {
    let (_dir, vault, rel) = vault_with_note("inbox2/not-inbox.md", "sibling").await;
    let result = attribute_and_build_event(&vault, &rel, &[], &default_scope())
        .await
        .expect("should not error");
    assert!(result.is_none(), "inbox2/ must not match the inbox/ prefix");
}

#[tokio::test]
async fn empty_capture_paths_drops_unflagged_notes() {
    // Fail-closed: an empty whitelist is not "react to everything". That was the pre-F12
    // leak. Empty paths still allow ready-flag and proposals.
    let (_dir, vault, rel) = vault_with_note("anywhere/note.md", "no flags").await;
    let scope = CaptureScope::new(Vec::<String>::new(), "#ready-now", "#hold-off");
    let result = attribute_and_build_event(&vault, &rel, &[], &scope)
        .await
        .expect("should not error");
    assert!(
        result.is_none(),
        "empty capture_paths must not let unflagged notes through"
    );
}

#[tokio::test]
async fn glob_capture_path_matches() {
    let (_dir, vault, rel) = vault_with_note("random/daily.md", "# daily thoughts").await;
    let scope = CaptureScope::new(["*.md"], "#ready-now", "#hold-off");
    let result = attribute_and_build_event(&vault, &rel, &[], &scope)
        .await
        .expect("should not error");
    assert!(
        result.is_some(),
        "path matching a glob capture_paths entry must produce an event"
    );
}

#[tokio::test]
async fn extra_capture_path_matches_pinned_file() {
    let (_dir, vault, rel) = vault_with_note("Inbox/Capture.md", "a widget jot").await;
    let scope = CaptureScope::new(["inbox/", "Inbox/Capture.md"], "#ready-now", "#hold-off");
    let result = attribute_and_build_event(&vault, &rel, &[], &scope)
        .await
        .expect("should not error");
    assert!(
        result.is_some(),
        "extra capture path Inbox/Capture.md must produce an event"
    );
}

#[tokio::test]
async fn ready_flag_override_uses_configured_spelling() {
    let (_dir, vault, rel) = vault_with_note("random/urgent.md", "# urgent thought #now").await;
    let scope = CaptureScope::new(["inbox/"], "#now", "#hold-off");
    let result = attribute_and_build_event(&vault, &rel, &[], &scope)
        .await
        .expect("should not error");
    assert!(
        result.is_some(),
        "path outside capture_paths but with the configured ready_flag must still produce an event"
    );
}

#[tokio::test]
async fn capture_paths_scope_respected_even_with_ignore_globs() {
    let (_dir, vault, rel) = vault_with_note("inbox/note.md", "# inbox note").await;
    let ignore_globs = vec!["*.sync-conflict-*".to_string()];
    let result = attribute_and_build_event(&vault, &rel, &ignore_globs, &default_scope())
        .await
        .expect("should not error");
    assert!(
        result.is_some(),
        "path matching capture_paths and not ignore_globs must produce an event"
    );
}

/// The `name()` string is the source identifier recorded on every event this source produces
/// (and is what surfaces in tracing when the source attaches). A wrong string silently tags
/// reactions as "from" the wrong origin, so pin it explicitly.
#[tokio::test]
async fn vault_event_source_name_is_vault_watch() {
    let (_dir, vault, _rel) = test_vault().await;
    let source = VaultEventSource::new(
        vault,
        Duration::from_millis(50),
        Vec::new(),
        default_scope(),
    );
    assert_eq!(source.name(), "vault-watch");
}

/// `matches_any_glob` must match a basename-only glob (`*.md`) against the file name even when
/// the full vault-relative path (`proposals/foo.md`) does not match (glob `*` does not cross
/// `/`). The `||` between the path and file-name matches is what enables this; a mutant that
/// flips it to `&&` would stop recognizing basename globs like `~*` or `*.tmp`.
#[test]
fn matches_any_glob_matches_basename_when_full_path_does_not() {
    assert!(
        matches_any_glob(
            std::path::Path::new("proposals/foo.md"),
            &["*.md".to_string()],
        ),
        "a basename glob must match via the file-name branch"
    );
    // A full-path pattern still matches.
    assert!(matches_any_glob(
        std::path::Path::new("proposals/foo.md"),
        &["proposals/*.md".to_string()],
    ));
    // An empty glob list never matches.
    assert!(!matches_any_glob(
        std::path::Path::new("proposals/foo.md"),
        &[]
    ));
}

/// `matches_capture_entry` routes glob patterns (`inbox/*`) to the glob branch and plain
/// folder prefixes (`inbox/`) to the prefix branch. The `||` in the glob-detection condition
/// must keep a `*` pattern on the glob branch; a mutant that flips it to `&&` would send
/// `inbox/*` to the prefix branch and misclassify it.
#[test]
fn matches_capture_entry_glob_branch_vs_prefix_branch() {
    assert!(
        matches_capture_entry(std::path::Path::new("inbox/foo.md"), "inbox/*"),
        "a glob pattern must take the glob branch and match"
    );
    assert!(matches_capture_entry(
        std::path::Path::new("inbox/foo.md"),
        "inbox/",
    ));
    assert!(
        !matches_capture_entry(std::path::Path::new("notes/foo.md"), "inbox/*"),
        "a non-matching glob must not match"
    );
    // A `?` glob with no `*` exercises the second `||` in the
    // glob-detection condition. A mutant that flips it to `&&` would send
    // `inbox/???.md` to the prefix branch and misclassify it.
    assert!(
        matches_capture_entry(std::path::Path::new("inbox/foo.md"), "inbox/???.md"),
        "a `?` glob must take the glob branch and match"
    );
    assert!(
        !matches_capture_entry(std::path::Path::new("notes/foo.md"), "inbox/???.md"),
        "a non-matching `?` glob must not match"
    );
}
