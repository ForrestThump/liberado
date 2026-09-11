#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
use tempfile::TempDir;

use super::*;
#[cfg(unix)]
use crate::pr_review::REVIEW_SCHEMA_VERSION;
use crate::pr_review::WorkerFailure;
use std::ffi::OsStr;

#[cfg(unix)]
struct RepoFixture {
    _dir: TempDir,
    root: PathBuf,
    base: String,
    head: String,
    other: String,
    schema: PathBuf,
}

#[cfg(unix)]
impl RepoFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).expect("repo dir");
        git(&root, &["init", "--quiet"]);
        git(
            &root,
            &["config", "user.email", "review-test@example.invalid"],
        );
        git(&root, &["config", "user.name", "Review Test"]);
        std::fs::write(root.join("file.txt"), "base\n").expect("base file");
        git(&root, &["add", "file.txt"]);
        git(&root, &["commit", "--quiet", "-m", "base"]);
        let base = rev_parse(&root, "HEAD");
        std::fs::write(root.join("file.txt"), "head\n").expect("head file");
        git(&root, &["commit", "--quiet", "-am", "head"]);
        let head = rev_parse(&root, "HEAD");
        git(&root, &["checkout", "--quiet", "--detach", &base]);
        std::fs::write(root.join("other.txt"), "other\n").expect("other file");
        git(&root, &["add", "other.txt"]);
        git(&root, &["commit", "--quiet", "-m", "other"]);
        let other = rev_parse(&root, "HEAD");
        git(&root, &["checkout", "--quiet", "--detach", &head]);
        let schema = root.join("review-schema.json");
        std::fs::write(&schema, REVIEW_RESULT_SCHEMA).expect("schema");
        git(&root, &["add", "review-schema.json"]);
        git(&root, &["commit", "--quiet", "-m", "schema"]);
        let head = rev_parse(&root, "HEAD");
        Self {
            _dir: dir,
            root,
            base,
            head,
            other,
            schema,
        }
    }

    fn request(&self) -> ReviewInvokeRequest {
        ReviewInvokeRequest {
            task_id: "pr-owner-repo-7".into(),
            command_id: "review-command".into(),
            run_id: "review-command".into(),
            repository: "owner/repo".into(),
            pr_number: 7,
            base_sha: self.base.clone(),
            expected_sha: self.head.clone(),
            workspace: self.root.clone(),
            schema_path: self.schema.clone(),
        }
    }
}

#[cfg(unix)]
fn git(root: &Path, args: &[&str]) {
    let output = liberado_common::process::std_command("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git starts");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
fn rev_parse(root: &Path, revision: &str) -> String {
    let output = liberado_common::process::std_command("git")
        .args(["rev-parse", revision])
        .current_dir(root)
        .output()
        .expect("rev-parse starts");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("utf8 sha")
        .trim()
        .into()
}

#[cfg(unix)]
fn fake_codex(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join("fake-codex.sh");
    std::fs::write(
        &path,
        r#"#!/bin/sh
if [ -n "${GITHUB_TOKEN+x}" ] || [ -n "${GH_TOKEN+x}" ] || [ -n "${GH_HOST+x}" ] || [ -n "${LIBERADO_GITHUB_TOKEN+x}" ] || [ -n "${GITLAB_TOKEN+x}" ]; then
  printf 'forge credentials leaked' >&2
  exit 9
fi
if [ -n "${CAPTURE_ARGS:-}" ]; then
  printf '%s\n' "$@" > "$CAPTURE_ARGS"
  pwd > "$CAPTURE_CWD"
fi
case "${FAKE_MODE:-success}" in
  malformed) printf '{}';;
  exhausted) printf "ERROR: You've hit your usage limit. Try again later\n" >&2; exit 1;;
  rate) printf 'HTTP 429: rate limit\n' >&2; exit 1;;
  auth) printf 'Unauthorized: authentication failed\n' >&2; exit 1;;
  mutate) printf 'changed\n' > worker-write.txt; printf '%s' "$FAKE_RESULT";;
  move_head) git checkout --quiet --detach "$FAKE_OTHER_SHA"; printf '%s' "$FAKE_RESULT";;
  *) printf '%s' "$FAKE_RESULT";;
esac
"#,
    )
    .expect("fake executable");
    let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("executable permissions");
    path
}

#[cfg(unix)]
fn valid_result(sha: &str) -> String {
    serde_json::json!({
        "schema": REVIEW_SCHEMA_VERSION,
        "reviewed_sha": sha,
        "summary": "clean",
        "findings": []
    })
    .to_string()
}

#[cfg(unix)]
fn port(fixture: &RepoFixture, mode: &str) -> CodexReviewPort {
    CodexReviewPort::new(fake_codex(fixture._dir.path()))
        .with_env("FAKE_MODE", mode)
        .with_env("FAKE_RESULT", &valid_result(&fixture.head))
        .with_env("FAKE_OTHER_SHA", &fixture.other)
}

#[cfg(unix)]
#[test]
fn invokes_codex_once_in_the_pinned_read_only_checkout() {
    let fixture = RepoFixture::new();
    let args = fixture._dir.path().join("args.txt");
    let cwd = fixture._dir.path().join("cwd.txt");
    let port = port(&fixture, "success")
        .with_env("CAPTURE_ARGS", &args.to_string_lossy())
        .with_env("CAPTURE_CWD", &cwd.to_string_lossy());
    let outcome = port.invoke(&fixture.request());
    assert!(matches!(outcome, ReviewInvokeOutcome::Finished { .. }));
    let argv = std::fs::read_to_string(args).expect("captured argv");
    assert!(argv.contains("--sandbox\nread-only\n"));
    assert!(argv.contains(&format!("--base\n{}\n", fixture.base)));
    assert!(argv.contains(&format!("--commit\n{}\n", fixture.head)));
    assert_eq!(
        std::fs::read_to_string(cwd).expect("captured cwd").trim(),
        fixture.root.to_string_lossy()
    );
}

#[cfg(unix)]
#[test]
fn strips_github_and_other_forge_credentials() {
    let fixture = RepoFixture::new();
    let port = port(&fixture, "success")
        .with_env("GITHUB_TOKEN", "must-not-reach-worker")
        .with_env("GH_TOKEN", "must-not-reach-worker")
        .with_env("GH_HOST", "must-not-reach-worker")
        .with_env("LIBERADO_GITHUB_TOKEN", "must-not-reach-worker")
        .with_env("GITLAB_TOKEN", "must-not-reach-worker");
    assert!(matches!(
        port.invoke(&fixture.request()),
        ReviewInvokeOutcome::Finished { .. }
    ));
}

#[cfg(unix)]
#[test]
fn zero_exit_with_malformed_output_fails() {
    let fixture = RepoFixture::new();
    assert!(matches!(
        port(&fixture, "malformed").invoke(&fixture.request()),
        ReviewInvokeOutcome::Failed { reason } if reason.contains("malformed")
    ));
}

#[cfg(unix)]
#[test]
fn opencode_port_accepts_a_schema_valid_result() {
    let fixture = RepoFixture::new();
    let port = OpenCodeReviewPort::new(
        fake_codex(fixture._dir.path()),
        crate::OPENCODE_NAMED_REVIEW_MODEL,
    )
    .with_env("FAKE_MODE", "success")
    .with_env("FAKE_RESULT", &valid_result(&fixture.head));
    assert!(matches!(
        port.invoke(&fixture.request()),
        ReviewInvokeOutcome::Finished { .. }
    ));
}

#[cfg(unix)]
#[test]
fn exact_codex_exhaustion_is_typed_unavailability_not_http_402() {
    let fixture = RepoFixture::new();
    assert_eq!(
        port(&fixture, "exhausted").invoke(&fixture.request()),
        ReviewInvokeOutcome::Unavailable {
            failure: WorkerFailure::Exhausted
        }
    );
    assert!(matches!(
        port(&fixture, "auth").invoke(&fixture.request()),
        ReviewInvokeOutcome::Failed { .. }
    ));
}

#[cfg(unix)]
#[test]
fn changed_head_and_changed_files_are_never_accepted() {
    let fixture = RepoFixture::new();
    assert_eq!(
        port(&fixture, "move_head").invoke(&fixture.request()),
        ReviewInvokeOutcome::Stale {
            expected: fixture.head.clone(),
            observed: fixture.other.clone(),
        }
    );
    git(
        &fixture.root,
        &["checkout", "--quiet", "--detach", &fixture.head],
    );
    assert!(matches!(
        port(&fixture, "mutate").invoke(&fixture.request()),
        ReviewInvokeOutcome::Failed { reason } if reason.contains("modified")
    ));
}

#[cfg(unix)]
#[test]
fn result_sha_must_match_request_and_workspace() {
    let fixture = RepoFixture::new();
    let port = CodexReviewPort::new(fake_codex(fixture._dir.path()))
        .with_env("FAKE_RESULT", &valid_result(&fixture.other));
    assert_eq!(
        port.invoke(&fixture.request()),
        ReviewInvokeOutcome::Stale {
            expected: fixture.head.clone(),
            observed: fixture.other.clone(),
        }
    );
}

#[test]
fn forge_environment_filter_covers_tokens_hosts_and_agent_credentials() {
    for name in [
        "GITHUB_TOKEN",
        "GITHUB_APP_PRIVATE_KEY",
        "GH_TOKEN",
        "GH_HOST",
        "LIBERADO_GITHUB_TOKEN",
        "GITLAB_ACCESS_TOKEN",
        "BITBUCKET_PASSWORD",
        "SSH_AUTH_SOCK",
        "GIT_ASKPASS",
    ] {
        assert!(is_forge_environment(OsStr::new(name)), "missed {name}");
    }
    assert!(!is_forge_environment(OsStr::new("CODEX_API_KEY")));
}
