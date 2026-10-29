//! `liberado shepherd --once` must fail when the daemon rejects `/api/goals`.
//!
//! This file is its own process. `liberado_cli::run` installs a process-global
//! tracing subscriber, so it cannot share a process with the success-path test.
//! `--dry-run` must not write to GitHub. The token is fake.

#[path = "common/runtime.rs"]
mod support;

use std::fs;
use std::net::TcpListener;
use std::thread;
use support::{
    EnvGuard, FAKE_TOKEN, assert_daemon_get, assert_dry_run_gh, serve_with, write_fake_gh,
};

#[tokio::test(flavor = "multi_thread")]
async fn shepherd_once_dry_run_reports_daemon_auth_failure() {
    let root = tempfile::tempdir().expect("temp repo");
    let config = tempfile::tempdir().expect("empty config");
    let bin = tempfile::tempdir().expect("fake gh dir");
    let gh_log = root.path().join("gh-argv.txt");
    fs::write(
        root.path().join("Cargo.toml"),
        "[workspace]\nmembers = []\n",
    )
    .expect("manifest");
    fs::create_dir(root.path().join("crates")).expect("crates dir");
    write_fake_gh(bin.path());

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind daemon double");
    let port = listener.local_addr().expect("local addr").port();
    let server = thread::spawn(move || {
        serve_with(
            listener,
            "401 Unauthorized",
            "{\"error\":\"unauthenticated\"}",
        )
    });

    let _env = EnvGuard::install(root.path(), config.path(), bin.path(), &gh_log, port);
    let mut args = [
        "shepherd".to_string(),
        "--once".to_string(),
        "--dry-run".to_string(),
    ]
    .into_iter();
    let result = liberado_cli::run(&mut args).await;
    drop(_env);

    let raw = server.join().expect("daemon double");
    let message = result
        .expect_err("401 must fail shepherd --once")
        .to_string();
    assert!(
        message.contains("LIBERADO_API_TOKEN"),
        "401 must name the API token variable"
    );
    assert!(message.contains("HTTP 401"));
    assert!(message.contains("present but rejected"));
    assert!(
        !message.contains(FAKE_TOKEN),
        "error included the token: {}",
        message.replace(FAKE_TOKEN, "[redacted]")
    );
    assert_daemon_get(&raw);
    assert_dry_run_gh(&fs::read_to_string(&gh_log).unwrap_or_default());
    let events =
        fs::read_to_string(root.path().join(".liberado/shepherd/events.jsonl")).unwrap_or_default();
    assert!(
        events.contains("daemon_goal_error"),
        "the auth failure must be in the event log"
    );
    assert!(
        !events.contains("pass_complete"),
        "an auth failure must stop the pass"
    );
    assert!(
        !events.contains(FAKE_TOKEN),
        "shepherd logs must not contain the API token: {}",
        events.replace(FAKE_TOKEN, "[redacted]")
    );
}
