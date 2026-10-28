//! `liberado shepherd` from the CLI's async entry point.
//!
//! `main` is `#[tokio::main]` and calls [`liberado_cli::run`]. Shepherd's daemon
//! client is `reqwest::blocking`, which owns a Tokio runtime. Dropping that
//! runtime on the async worker panics with "Cannot drop a runtime in a context
//! where blocking is not allowed" before the request leaves. This test drives
//! that same `run` from a multi-thread runtime, against a local daemon double.
//!
//! The binary is this file alone, so the environment edits stay inside one
//! process. `--dry-run` must not write to GitHub. The token below is fake.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

const FAKE_TOKEN: &str = "fake-shepherd-token-111";

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
    let server = thread::spawn(move || serve(listener));

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

fn assert_daemon_get(raw: &str) {
    assert!(
        raw.lines().any(|line| line.starts_with("GET /api/goals ")),
        "dry-run must GET /api/goals while choosing a review slot"
    );
    assert!(
        !raw.lines().any(|line| line.starts_with("POST ")),
        "dry-run must not start a goal"
    );
    let bearer = format!("Bearer {FAKE_TOKEN}");
    let found = raw.lines().any(|line| {
        let Some((name, value)) = line.split_once(':') else {
            return false;
        };
        name.eq_ignore_ascii_case("authorization") && value.trim() == bearer
    });
    assert!(found, "daemon GET must send the configured bearer");
}

fn assert_dry_run_gh(log: &str) {
    assert!(
        log.contains("pr list"),
        "shepherd should list pull requests through the local gh double"
    );
    assert!(
        !log.contains(FAKE_TOKEN),
        "gh must not receive the Liberado API token"
    );
    for line in log.lines() {
        let mut parts = line.split_whitespace();
        let command = parts.next().unwrap_or("");
        let sub = parts.next().unwrap_or("");
        let write = matches!(
            (command, sub),
            ("label", _)
                | (
                    "pr",
                    "edit" | "comment" | "close" | "merge" | "create" | "review"
                )
                | ("run", "rerun" | "cancel")
                | ("api", _)
        );
        let shown = line.replace(FAKE_TOKEN, "[redacted]");
        assert!(!write, "dry-run must not write to GitHub: {shown}");
    }
}

fn serve(listener: TcpListener) -> String {
    listener.set_nonblocking(true).expect("nonblocking");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut raw = String::new();
    let mut idle_after: Option<Instant> = None;
    while Instant::now() < deadline {
        if idle_after.is_some_and(|at| Instant::now() >= at) {
            break;
        }
        match listener.accept() {
            Ok((mut sock, _)) => {
                sock.set_nonblocking(false).expect("blocking socket");
                sock.set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("read timeout");
                raw.push_str(&read_request(&mut sock));
                raw.push('\n');
                let _ = sock.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]",
                );
                idle_after = Some(Instant::now() + Duration::from_millis(250));
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }
    raw
}

fn read_request(sock: &mut std::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0_u8; 1024];
    loop {
        match sock.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Build a `gh` executable. Windows `Command` resolves `gh` to `gh.exe` and will not
/// prefer a `.cmd` double over a real `gh.exe` later on `PATH`.
fn write_fake_gh(dir: &Path) {
    let source = dir.join("gh_fake.rs");
    fs::write(&source, FAKE_GH_RS).expect("fake gh source");
    let exe = dir.join(if cfg!(windows) { "gh.exe" } else { "gh" });
    let output = liberado_common::process::std_command("rustc")
        .arg("-o")
        .arg(&exe)
        .arg(&source)
        .output()
        .expect("run rustc");
    assert!(
        output.status.success(),
        "rustc fake gh failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

const FAKE_GH_RS: &str = r#"
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Ok(path) = std::env::var("SHEPHERD_GH_LOG") {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{}", args.join(" "));
        }
    }
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let sub = args.get(1).map(String::as_str).unwrap_or("");
    match (cmd, sub) {
        ("pr", "list") => println!(
            "[{{\"number\":7,\"title\":\"runtime panic\",\"headRefName\":\"topic\",\"baseRefOid\":\"aaaaaaaaaaaa\",\"headRefOid\":\"bbbbbbbbbbbb\",\"url\":\"http://127.0.0.1/pull/7\",\"labels\":[],\"isDraft\":false}}]"
        ),
        ("pr", "checks") => println!("[{{\"name\":\"ci\",\"state\":\"success\"}}]"),
        ("run", "list") => println!("[]"),
        _ => {
            eprintln!("unexpected gh {}", args.join(" "));
            std::process::exit(2);
        }
    }
}
"#;

/// Restores process environment when dropped. This test file is its own binary.
struct EnvGuard {
    cwd: PathBuf,
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn install(root: &Path, config: &Path, bin: &Path, gh_log: &Path, port: u16) -> Self {
        let cwd = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(root).expect("chdir temp repo");
        let mut paths = vec![bin.to_path_buf()];
        if let Some(path) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&path));
        }
        let path = std::env::join_paths(paths).expect("PATH");
        let saved_keys = [
            "PATH",
            "LIBERADO_CONFIG_DIR",
            "LIBERADO_SERVER",
            "LIBERADO_API_TOKEN",
            "SHEPHERD_GH_LOG",
            "SHEPHERD_COLD_REVIEWS",
            "SHEPHERD_MAX_KICKBACKS",
            "SHEPHERD_MAX_CONCURRENT",
            "SHEPHERD_POLL_SECONDS",
            "SHEPHERD_COLD_REVIEW_MAX_TURNS",
            "SHEPHERD_BASE",
            "SHEPHERD_PROJECT",
            "SHEPHERD_PROFILE",
        ];
        let saved = saved_keys
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
        set_var("PATH", &path);
        set_var("LIBERADO_CONFIG_DIR", config);
        set_var("LIBERADO_SERVER", format!("http://127.0.0.1:{port}"));
        set_var("LIBERADO_API_TOKEN", FAKE_TOKEN);
        set_var("SHEPHERD_GH_LOG", gh_log);
        set_var("SHEPHERD_COLD_REVIEWS", "2");
        set_var("SHEPHERD_MAX_KICKBACKS", "2");
        set_var("SHEPHERD_MAX_CONCURRENT", "2");
        set_var("SHEPHERD_POLL_SECONDS", "30");
        set_var("SHEPHERD_COLD_REVIEW_MAX_TURNS", "60");
        set_var("SHEPHERD_BASE", "main");
        set_var("SHEPHERD_PROJECT", "liberado");
        set_var("SHEPHERD_PROFILE", "coding-unattended");
        Self { cwd, saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            match value {
                Some(value) => set_var(key, value),
                None => remove_var(key),
            }
        }
        let _ = std::env::set_current_dir(&self.cwd);
    }
}

fn set_var(key: &str, value: impl AsRef<OsStr>) {
    unsafe { std::env::set_var(key, value) }
}

fn remove_var(key: &str) {
    unsafe { std::env::remove_var(key) }
}
