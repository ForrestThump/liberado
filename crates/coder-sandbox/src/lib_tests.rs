//! Split from `lib.rs` for module-health boundaries.

/// `Drop` does `remove_dir_all(worktree_path)`, so a traversing session id would delete outside
/// the worktree base. Ids are internally minted ULIDs today — this is what keeps that a fact
/// rather than an assumption.
#[tokio::test]
async fn a_traversing_session_id_is_refused_before_any_directory_is_made() {
    let base = std::env::temp_dir().join(format!("wt-guard-{}", std::process::id()));
    for bad in ["../escape", "a/b", "..", ""] {
        let err = WorktreeWorkspace::new(
            std::path::Path::new("."),
            bad,
            &base,
            CommandPolicy::default(),
        )
        .await;
        assert!(
            err.is_err(),
            "session id {bad:?} must be refused, not joined into a path Drop will delete"
        );
    }
    assert!(!base.exists(), "a refused id must not create the base dir");
}

use super::*;

fn workspace() -> (tempfile::TempDir, HostWorkspace) {
    let dir = tempfile::tempdir().unwrap();
    let ws = HostWorkspace::new(dir.path(), CommandPolicy::default()).unwrap();
    (dir, ws)
}

#[test]
fn resolve_path_rejects_parent_escape() {
    let (_dir, ws) = workspace();
    let err = ws.resolve_path("../Cargo.toml").unwrap_err();
    assert!(matches!(err, SandboxError::PathEscape(_)));
}

/// What counts as absolute is platform-specific, and the guard is `Path::is_absolute`, so the
/// test has to speak the host's dialect. `C:/Windows` is absolute on Windows but an ordinary
/// relative name on Unix — hardcoding it passed here and failed on Linux, where the path was
/// simply joined onto the root instead of refused.
#[test]
fn resolve_path_rejects_absolute_path() {
    let (_dir, ws) = workspace();
    let absolute = if cfg!(windows) { "C:/Windows" } else { "/etc" };
    let err = ws.resolve_path(absolute).unwrap_err();
    assert!(matches!(err, SandboxError::AbsolutePath(_)));
}

/// The other half of that asymmetry: on Unix a drive-letter path is not absolute, so it is
/// treated as an ordinary relative name. That is safe — it still lands inside the root — but
/// pinning it keeps the behaviour deliberate rather than incidental.
#[cfg(unix)]
#[test]
fn drive_letter_path_is_contained_on_unix() {
    let (_dir, ws) = workspace();
    let path = ws.resolve_path("C:/Windows").unwrap();
    assert!(path.starts_with(ws.root()));
}

#[test]
fn resolve_path_accepts_nested_relative_path() {
    let (_dir, ws) = workspace();
    let path = ws.resolve_path("src/lib.rs").unwrap();
    assert!(path.ends_with(Path::new("src/lib.rs")));
    assert!(path.starts_with(ws.root()));
}

#[test]
fn command_policy_denies_prefix_match() {
    let policy = CommandPolicy {
        allow: vec!["cargo test".to_string()],
        deny: vec!["cargo publish".to_string()],
        ..CommandPolicy::default()
    };
    let mut request = CommandRequest::new("cargo");
    request.args = vec!["publish".to_string(), "--dry-run".to_string()];
    assert!(ensure_command_allowed(&policy, &request).is_err());
}

#[test]
fn command_policy_allows_configured_prefix() {
    let policy = CommandPolicy {
        allow: vec!["cargo test".to_string()],
        ..CommandPolicy::default()
    };
    let mut request = CommandRequest::new("cargo");
    request.args = vec!["test".to_string(), "-p".to_string(), "x".to_string()];
    assert!(ensure_command_allowed(&policy, &request).is_ok());
}

/// Backlog item C1: an empty `allow` list means "allow all", so without a default deny
/// `run_command` could invoke git with no capability check at all. The default policy must
/// refuse git, in every argv spelling — including the Windows binary name.
#[test]
fn a_grant_lets_a_denied_program_run_the_policy_check() {
    let grants = CommandGrantSet::default();
    grants.allow("git");
    assert!(grants.contains("git"));
    assert!(grants.contains("git.exe"));
    assert!(grants.contains(r"C:\Program Files\Git\cmd\git.exe"));
    grants.revoke("GIT.EXE");
    assert!(!grants.contains("git"));
}

#[tokio::test]
async fn granting_git_bypasses_the_default_deny_on_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let grants = CommandGrantSet::default();
    grants.allow("git");
    let ws = HostWorkspace::new(dir.path(), CommandPolicy::default())
        .unwrap()
        .with_command_grants(grants);
    // `git --version` is a no-op for the repo; we only care that policy did not refuse it.
    let mut request = CommandRequest::new("git");
    request.args = vec!["--version".into()];
    let out = ws
        .run_command(request)
        .await
        .expect("a granted git must pass the deny list");
    assert_eq!(out.exit_code, Some(0), "git --version should run: {out:?}");
}

#[test]
fn default_policy_denies_git() {
    let policy = CommandPolicy::default();
    for (program, args) in [
        ("git", vec!["status".to_string(), "--porcelain".to_string()]),
        ("git", vec!["push".to_string()]),
        ("git.exe", vec!["status".to_string()]),
        ("GIT", vec!["status".to_string()]),
    ] {
        let mut request = CommandRequest::new(program);
        request.args = args.clone();
        assert!(
            ensure_command_allowed(&policy, &request).is_err(),
            "default policy must refuse git: {program} {args:?}"
        );
    }
}

/// A configured deny rule still beats an allow rule, and the prefix semantics are unchanged:
/// `deny: ["cargo publish"]` denies `cargo publish --dry-run` but not `cargo build`.
#[test]
fn command_policy_deny_still_wins_over_allow() {
    let policy = CommandPolicy {
        allow: vec!["cargo".to_string()],
        deny: vec!["cargo publish".to_string()],
        ..CommandPolicy::default()
    };
    let mut request = CommandRequest::new("cargo");
    request.args = vec!["publish".to_string(), "--dry-run".to_string()];
    assert!(ensure_command_allowed(&policy, &request).is_err());

    let mut build = CommandRequest::new("cargo");
    build.args = vec!["build".to_string()];
    assert!(ensure_command_allowed(&policy, &build).is_ok());
}

/// Windows: the model may name the program `git.exe` (or a full path to it). The stem match
/// must catch it even though the full command line "git.exe status" does not start with "git ".
#[test]
fn deny_matches_windows_git_exe_stem() {
    assert!(deny_matches_program_stem("git", "git.exe"));
    assert!(deny_matches_program_stem(
        "git",
        "C:\\Program Files\\Git\\bin\\git.exe"
    ));
    assert!(deny_matches_program_stem("git", "GIT.EXE"));
    assert!(deny_matches_program_stem("git", "/usr/bin/git"));
    assert!(deny_matches_program_stem(
        "git",
        "C:/Program Files/Git/cmd/git.exe"
    ));
    assert!(!deny_matches_program_stem("git", "gitty"));
    assert!(!deny_matches_program_stem("git status", "git"));
    assert!(!deny_matches_program_stem("", "git"));
}

/// `Path::file_stem` on Unix treats a Windows path as one filename. The helper
/// must not.
#[test]
fn program_stem_splits_on_backslash_even_on_unix() {
    assert_eq!(
        program_file_stem(r"C:\Program Files\Git\bin\git.exe"),
        "git"
    );
    assert_eq!(program_file_stem("/usr/bin/git"), "git");
    assert_eq!(program_file_stem("GIT.EXE"), "GIT");
    assert_eq!(program_file_stem("gitty"), "gitty");
}

#[test]
fn preview_or_offload_writes_full_output_and_returns_head_tail() {
    let dir = tempfile::tempdir().unwrap();
    let big = "0123456789abcdef".repeat(8); // 128 chars
    let (preview, offload) = preview_or_offload(
        big.clone().into_bytes(),
        32,
        Some(dir.path()),
        "cmd-x-stdout.txt",
    );
    let path = offload.expect("must return an offload path");
    let written = std::fs::read_to_string(&path).unwrap();
    assert_eq!(written, big, "offload file must hold the full body");
    assert!(
        preview.starts_with(&big[..16]),
        "preview must start with the head half, got: {preview:?}"
    );
    assert!(
        preview.ends_with(&big[112..]),
        "preview must end with the tail half, got: {preview:?}"
    );
    assert!(
        preview.contains("truncated"),
        "preview must name the omission"
    );
}

#[test]
fn preview_or_offload_passes_through_at_exact_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let text = "abc".to_string();
    let (preview, offload) =
        preview_or_offload(text.clone().into_bytes(), 3, Some(dir.path()), "f.txt");
    assert_eq!(preview, text);
    assert!(offload.is_none(), "no offload file for in-bounds output");
}

#[test]
fn preview_or_offload_passes_through_below_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let text = "ab".to_string();
    let (preview, offload) =
        preview_or_offload(text.clone().into_bytes(), 3, Some(dir.path()), "f.txt");
    assert_eq!(preview, text);
    assert!(offload.is_none(), "no offload file for in-bounds output");
}

#[test]
fn preview_or_offload_falls_back_to_head_truncation_without_dir() {
    let text = "abcdef".to_string();
    let (preview, offload) = preview_or_offload(text.clone().into_bytes(), 3, None, "f.txt");
    assert_eq!(preview, "abc", "no offload dir -> head truncation");
    assert!(offload.is_none());
}

#[test]
fn preview_or_offload_falls_back_when_dir_write_fails() {
    // A *file* occupying the offload path makes `create_dir_all` fail on every platform.
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, "file, not dir").unwrap();

    let text = "abcdef".to_string();
    let (preview, offload) =
        preview_or_offload(text.clone().into_bytes(), 3, Some(&blocker), "f.txt");
    assert_eq!(preview, "abc", "write failure -> head truncation");
    assert!(offload.is_none());
}

#[test]
fn preview_or_offload_decodes_utf16_le_without_nuls() {
    // PowerShell / some cmd builtins emit UTF-16 LE. `from_utf8_lossy` keeps the NULs
    // (`W\0i\0n\0d\0o\0w\0s`) and the model cannot read the tool result.
    let mut utf16 = Vec::new();
    for unit in "Windows PowerShell".encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }
    let (text, offload) = preview_or_offload(utf16, 1024, None, "f.txt");
    assert_eq!(text, "Windows PowerShell");
    assert!(
        !text.contains('\0'),
        "decoded command output must not keep UTF-16 NULs: {text:?}"
    );
    assert!(offload.is_none(), "under threshold -> no offload");
}

#[test]
fn preview_or_offload_decodes_utf16_le_bom() {
    let mut utf16 = vec![0xFF, 0xFE];
    for unit in "hi".encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }
    let (text, _) = preview_or_offload(utf16, 1024, None, "f.txt");
    assert_eq!(text, "hi");
}

#[test]
fn preview_or_offload_keeps_utf8() {
    let (text, _) = preview_or_offload("café".as_bytes().to_vec(), 1024, None, "f.txt");
    assert_eq!(text, "café");
}

#[test]
fn resolve_path_accepts_curdir_prefix() {
    let (_dir, ws) = workspace();
    let path = ws.resolve_path("./src/lib.rs").unwrap();
    assert!(path.ends_with(Path::new("src/lib.rs")));
}

#[test]
fn resolve_path_accepts_intermediate_curdir() {
    let (_dir, ws) = workspace();
    let path = ws.resolve_path("src/./lib.rs").unwrap();
    assert!(path.ends_with(Path::new("lib.rs")));
}

#[test]
fn docker_workspace_builds_docker_run_args() {
    let dir = tempfile::tempdir().unwrap();
    let ws = DockerWorkspace::new(
        dir.path(),
        DockerSandboxSpec {
            image: "liberado-coder:latest".to_string(),
            network: Some("none".to_string()),
            env_allowlist: vec!["OPENROUTER_API_KEY".to_string()],
            volumes: vec![SandboxVolume {
                host: "C:\\cache".to_string(),
                container: "/cache".to_string(),
                read_only: true,
            }],
            user: Some("1000:1000".to_string()),
        },
        CommandPolicy::default(),
    )
    .unwrap();
    let mut request = CommandRequest::new("cargo");
    request.args = vec!["test".to_string()];
    request
        .env
        .insert("RUST_LOG".to_string(), "info".to_string());

    let args = ws.docker_run_args(&request).unwrap();

    assert_eq!(args[0], "run");
    assert!(args.contains(&"--rm".to_string()));
    assert!(args.contains(&"--network".to_string()));
    assert!(args.contains(&"none".to_string()));
    assert!(args.contains(&"--user".to_string()));
    assert!(args.contains(&"1000:1000".to_string()));
    assert!(args.contains(&"OPENROUTER_API_KEY".to_string()));
    assert!(args.contains(&"RUST_LOG=info".to_string()));
    assert!(args.contains(&"C:/cache:/cache:ro".to_string()));
    assert_eq!(
        args.iter().rev().take(3).cloned().collect::<Vec<_>>(),
        vec![
            "test".to_string(),
            "cargo".to_string(),
            "liberado-coder:latest".to_string()
        ]
    );

    // Workspace volume mount at args[4] must reference /workspace.
    assert!(
        args[4].contains(":/workspace"),
        "volume mount should reference /workspace, got: {0}",
        args[4]
    );
    assert!(
        !args[4].contains(":ro"),
        "volume mount should not be read-only, got: {0}",
        args[4]
    );
    let normalized_root = ws.root().to_string_lossy().replace('\\', "/");
    assert!(
        args[4].starts_with(&normalized_root),
        "volume mount should start with host root, got: {0}",
        args[4]
    );
}

#[test]
fn docker_workspace_resolve_path_delegates_to_host() {
    let dir = tempfile::tempdir().unwrap();
    let ws = DockerWorkspace::new(
        dir.path(),
        DockerSandboxSpec {
            image: "liberado-coder:latest".to_string(),
            network: None,
            env_allowlist: Vec::new(),
            volumes: Vec::new(),
            user: None,
        },
        CommandPolicy::default(),
    )
    .unwrap();
    let path = ws.resolve_path("src/lib.rs").unwrap();
    assert!(path.ends_with(Path::new("src/lib.rs")));
    assert!(path.starts_with(ws.root()));
}

#[test]
fn docker_workspace_obeys_command_policy() {
    let dir = tempfile::tempdir().unwrap();
    let ws = DockerWorkspace::new(
        dir.path(),
        DockerSandboxSpec {
            image: "liberado-coder:latest".to_string(),
            network: None,
            env_allowlist: Vec::new(),
            volumes: Vec::new(),
            user: None,
        },
        CommandPolicy {
            allow: vec!["cargo test".to_string()],
            ..CommandPolicy::default()
        },
    )
    .unwrap();
    let mut request = CommandRequest::new("cargo");
    request.args = vec!["publish".to_string()];

    let err = ws.docker_run_args(&request).unwrap_err();

    assert!(matches!(err, SandboxError::CommandDenied(_)));
}

// ── WorktreeWorkspace tests ─────────────────────────────────────────

#[test]
fn strip_extended_path_prefix_removes_verbatim_drive_and_unc() {
    #[cfg(windows)]
    {
        assert_eq!(
            strip_extended_path_prefix(Path::new(r"\\?\C:\Users\me\repo")),
            PathBuf::from(r"C:\Users\me\repo")
        );
        assert_eq!(
            strip_extended_path_prefix(Path::new(r"\\?\UNC\server\share\repo")),
            PathBuf::from(r"\\server\share\repo")
        );
        assert_eq!(
            strip_extended_path_prefix(Path::new(r"//?/C:/Users/me/repo")),
            PathBuf::from(r"C:\Users\me\repo")
        );
    }
    // Already-plain paths are unchanged on every platform.
    assert_eq!(
        strip_extended_path_prefix(Path::new("/home/me/repo")),
        PathBuf::from("/home/me/repo")
    );
}

async fn worktree_setup() -> (tempfile::TempDir, tempfile::TempDir, WorktreeWorkspace) {
    let parent = tempfile::tempdir().unwrap();
    let base = tempfile::tempdir().unwrap();

    let status = liberado_common::process::std_command("git")
        .args(["init", "--quiet"])
        .current_dir(parent.path())
        .status()
        .unwrap();
    assert!(status.success());

    // A commit is needed for worktree to have a HEAD to check out.
    std::fs::write(parent.path().join("README.md"), "# test repo\n").unwrap();
    let status = liberado_common::process::std_command("git")
        .args(["-C", &parent.path().to_string_lossy()])
        .args(["add", "README.md"])
        .status()
        .unwrap();
    assert!(status.success());
    let status = liberado_common::process::std_command("git")
        .args(["-C", &parent.path().to_string_lossy()])
        .args(["commit", "--quiet", "-m", "init"])
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .status()
        .unwrap();
    assert!(status.success());

    let ws = WorktreeWorkspace::new(
        parent.path(),
        "test-session",
        base.path(),
        CommandPolicy::default(),
    )
    .await
    .unwrap();
    (parent, base, ws)
}

#[tokio::test]
async fn worktree_root_is_a_subdirectory_of_the_base() {
    let (_parent, _base, ws) = worktree_setup().await;
    assert!(ws.root().exists(), "worktree root must exist");
    assert!(
        ws.root().join("README.md").exists(),
        "worktree must have parent's committed files"
    );
    // The root is at <base>/test-session. On some platforms canonicalize
    // resolves tempdir symlinks differently, so check by relative path.
    assert_eq!(ws.root().file_name().unwrap(), "test-session");
}

#[tokio::test]
async fn worktree_writes_are_isolated_from_parent() {
    let (parent, _base, ws) = worktree_setup().await;

    let parent_readme = std::fs::read_to_string(parent.path().join("README.md")).unwrap();
    assert_eq!(parent_readme, "# test repo\n");

    std::fs::write(ws.root().join("new-file.txt"), "worktree content").unwrap();
    assert!(ws.root().join("new-file.txt").exists());
    assert!(
        !parent.path().join("new-file.txt").exists(),
        "worktree write must not appear in parent"
    );

    let parent_readme2 = std::fs::read_to_string(parent.path().join("README.md")).unwrap();
    assert_eq!(parent_readme2, "# test repo\n", "parent README untouched");
}

#[tokio::test]
async fn worktree_cleanup_removes_the_directory() {
    let (_parent, _base, mut ws) = worktree_setup().await;
    let root = ws.root().to_path_buf();
    assert!(root.exists());
    ws.cleanup().await;
    assert!(!root.exists());
}

#[tokio::test]
async fn worktree_drop_removes_the_directory() {
    let (_parent, _base, ws) = worktree_setup().await;
    let root = ws.root().to_path_buf();
    assert!(root.exists());
    drop(ws);
    assert!(!root.exists());
}

#[tokio::test]
async fn durable_session_worktree_reuses_path_and_survives_drop() {
    let parent = tempfile::tempdir().unwrap();
    let base = tempfile::tempdir().unwrap();
    let status = liberado_common::process::std_command("git")
        .args(["init", "--quiet"])
        .current_dir(parent.path())
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::write(parent.path().join("seed.txt"), "v1\n").unwrap();
    let _ = liberado_common::process::std_command("git")
        .args(["-C", &parent.path().to_string_lossy()])
        .args(["add", "seed.txt"])
        .status();
    let _ = liberado_common::process::std_command("git")
        .args(["-C", &parent.path().to_string_lossy()])
        .args(["commit", "--quiet", "-m", "init"])
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .status();

    let p1 = ensure_session_worktree(parent.path(), "sess-durable", base.path())
        .await
        .unwrap();
    std::fs::write(p1.join("marker.txt"), "kept\n").unwrap();
    let p2 = ensure_session_worktree(parent.path(), "sess-durable", base.path())
        .await
        .unwrap();
    assert_eq!(p1, p2, "second ensure must reuse the same path");
    assert_eq!(
        std::fs::read_to_string(p2.join("marker.txt")).unwrap(),
        "kept\n",
        "durable worktree must not wipe in-progress edits on re-ensure"
    );
}

#[tokio::test]
async fn worktree_resolve_path_is_contained() {
    let (_parent, _base, ws) = worktree_setup().await;
    let path = ws.resolve_path("src/lib.rs").unwrap();
    assert!(path.starts_with(ws.root()));
}

#[tokio::test]
async fn worktree_resolve_path_rejects_escape() {
    let (_parent, _base, ws) = worktree_setup().await;
    let err = ws.resolve_path("../secret.txt").unwrap_err();
    assert!(matches!(err, SandboxError::PathEscape(_)));
}

/// After explicit cleanup with prune, a second worktree for the same session id
/// can be created without git complaining about an existing registration.
#[tokio::test]
async fn worktree_recreation_after_cleanup_succeeds() {
    let parent = tempfile::tempdir().unwrap();
    let base = tempfile::tempdir().unwrap();

    let status = liberado_common::process::std_command("git")
        .args(["init", "--quiet"])
        .current_dir(parent.path())
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::write(parent.path().join("f"), "v").unwrap();
    let _ = liberado_common::process::std_command("git")
        .args(["-C", &parent.path().to_string_lossy()])
        .args(["add", "."])
        .status()
        .unwrap();
    let _ = liberado_common::process::std_command("git")
        .args(["-C", &parent.path().to_string_lossy()])
        .args(["commit", "--quiet", "-m", "x"])
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .status()
        .unwrap();

    let session = "recreate-test";
    let mut ws1 = WorktreeWorkspace::new(
        parent.path(),
        session,
        base.path(),
        CommandPolicy::default(),
    )
    .await
    .unwrap();
    assert!(ws1.root().exists());
    ws1.cleanup().await;
    assert!(!ws1.root().exists());

    // Second creation must succeed — prune cleared the stale registration.
    let ws2 = WorktreeWorkspace::new(
        parent.path(),
        session,
        base.path(),
        CommandPolicy::default(),
    )
    .await
    .unwrap();
    assert!(ws2.root().exists());
    drop(ws2);
}
