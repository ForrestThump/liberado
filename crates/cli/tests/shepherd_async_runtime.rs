//! `liberado shepherd` from the CLI's async entry point.
//!
//! `main` is `#[tokio::main]` and calls [`liberado_cli::run`]. Shepherd's daemon
//! client is `reqwest::blocking`, which owns a Tokio runtime. Dropping that
//! runtime on the async worker panics with "Cannot drop a runtime in a context
//! where blocking is not allowed" before the request leaves. This test drives
//! that same `run` from a multi-thread runtime, against a local daemon double.
//!
//! This file is its own process. `run` installs a process-global tracing
//! subscriber, so the auth-failure case lives in `shepherd_auth_failure.rs`.
//! `--dry-run` must not write to GitHub. The token below is fake.

#[path = "common/runtime.rs"]
mod support;

use std::fs;
use std::net::TcpListener;
use std::thread;
use support::{
    EnvGuard, FAKE_TOKEN, assert_daemon_get, assert_dry_run_gh, serve_with, write_fake_gh,
};

#[tokio::test(flavor = "multi_thread")]
async fn shepherd_once_dry_run_queries_the_daemon_from_the_async_cli() {
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
    let server = thread::spawn(move || serve_with(listener, "200 OK", "[]"));

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
    result.expect("shepherd --once --dry-run");
    assert_daemon_get(&raw);
    assert_dry_run_gh(&fs::read_to_string(&gh_log).unwrap_or_default());
    let events =
        fs::read_to_string(root.path().join(".liberado/shepherd/events.jsonl")).unwrap_or_default();
    assert!(
        events.contains("pass_complete"),
        "one dry-run pass should finish"
    );
    assert!(
        !events.contains(FAKE_TOKEN),
        "shepherd logs must not contain the API token"
    );
}
