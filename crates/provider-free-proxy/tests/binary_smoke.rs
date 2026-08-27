//! Black-box tests for the `liberado-free-proxy` binary.
//!
//! The library modules are covered by unit and seam tests; these cover the process contract:
//! a missing credential refuses to boot with a clear message (exit 2), and a booted proxy
//! binds, logs, and serves `/healthz`. Together they kill the classic replace-`main`-with-()
//! survivors that in-process tests cannot reach.
//!
//! Hermeticity rules for the spawned child:
//! - `LIBERADO_FREE_PROXY_UPSTREAM_BASE` points at a closed loopback port, so ranking fails
//!   **instantly** (connection refused) instead of reaching for the real network;
//! - `LIBERADO_FREE_PROXY_BIND` names a port the test pre-checked as free, so the address is
//!   known without parsing logs (the log itself is still drained and asserted).

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct KillOnDrop(Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Drain the child's stderr on a background thread so the pipe can never fill and block it;
/// the collected lines are asserted afterwards.
fn drain_stderr(child: &mut Child) -> Arc<Mutex<Vec<String>>> {
    let stderr = child.stderr.take().expect("stderr piped");
    let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            sink.lock().expect("drain lock").push(line);
        }
    });
    lines
}

/// Grab an ephemeral port by binding and releasing it. There is a small reserve-and-race
/// window; the healthz poll treats connection-refused like any other not-ready signal.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("ephemeral bind")
        .local_addr()
        .expect("local addr")
        .port()
}

fn clear_provider_keys(cmd: &mut Command) {
    for name in liberado_provider_free_proxy::listed_key_env_names() {
        cmd.env_remove(name);
    }
}

fn spawn_proxy(port: u16) -> (KillOnDrop, Arc<Mutex<Vec<String>>>) {
    spawn_proxy_with(port, &[("OPENROUTER_API_KEY", "sk-smoke")])
}

fn spawn_proxy_with(port: u16, keys: &[(&str, &str)]) -> (KillOnDrop, Arc<Mutex<Vec<String>>>) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_liberado-free-proxy"));
    clear_provider_keys(&mut cmd);
    for (k, v) in keys {
        cmd.env(k, v);
    }
    let mut child = cmd
        .env("LIBERADO_FREE_PROXY_BIND", format!("127.0.0.1:{port}"))
        // Closed port on loopback: refuses immediately, keeps the test off the network.
        .env(
            "LIBERADO_FREE_PROXY_UPSTREAM_BASE",
            "http://127.0.0.1:9/api/v1",
        )
        .env_remove("SPIDER_MCP_URL") // keep the scrape fallback out of the test too
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    let drained = drain_stderr(&mut child);
    (KillOnDrop(child), drained)
}

fn wait_for_healthz(port: u16) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    let addr = format!("127.0.0.1:{port}");
    while Instant::now() < deadline {
        if let Ok(body) = http_get(&addr, "/healthz") {
            return body;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("proxy never answered /healthz on {addr}");
}

fn wait_for_log(lines: &Arc<Mutex<Vec<String>>>, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if lines
            .lock()
            .expect("drain lock")
            .iter()
            .any(|l| l.contains(needle))
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "stderr never mentioned {needle:?}; captured: {:?}",
        lines.lock().expect("drain lock")
    );
}

/// Minimal HTTP/1.1 GET by hand: `/healthz` is one word, and pulling an HTTP client into
/// dev-deps for it would be the wrong trade. Connection failures surface as `Err`.
fn http_get(addr: &str, path: &str) -> Result<String, String> {
    let raw = (|| -> std::io::Result<String> {
        let mut stream = std::net::TcpStream::connect(addr)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
        )?;
        let mut raw = String::new();
        stream.read_to_string(&mut raw)?;
        Ok(raw)
    })()
    .map_err(|e| format!("request failed: {e}"))?;

    let (head, body) = raw.split_once("\r\n\r\n").ok_or("no header/body split")?;
    if head.starts_with("HTTP/1.1 200") || head.starts_with("HTTP/1.0 200") {
        Ok(body.trim().to_string())
    } else {
        Err(format!("non-200 head: {head}"))
    }
}

#[test]
fn missing_all_provider_keys_exits_2_listing_the_env_names() {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_liberado-free-proxy"));
    clear_provider_keys(&mut cmd);
    let output = cmd.output().expect("binary spawns");
    assert_eq!(
        output.status.code(),
        Some(2),
        "exit code must flag misconfiguration"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("OPENROUTER_API_KEY"), "{stderr}");
    assert!(stderr.contains("GROQ_API_KEY"), "{stderr}");
    assert!(stderr.contains("GEMINI_API_KEY"), "{stderr}");
    assert!(stderr.contains("KILOCODE_API_KEY"), "{stderr}");
    assert!(
        !stderr.contains("sk-"),
        "must not print key-shaped values: {stderr}"
    );
}

#[test]
fn groq_key_alone_is_enough_to_boot() {
    let port = free_port();
    let (_child, drained) = spawn_proxy_with(
        port,
        &[
            ("GROQ_API_KEY", "gsk-smoke"),
            (
                "LIBERADO_FREE_PROXY_GROQ_BASE",
                "http://127.0.0.1:9/openai/v1",
            ),
        ],
    );
    let body = wait_for_healthz(port);
    assert_eq!(body, "ok");
    wait_for_log(&drained, "listening");
}

#[test]
fn booted_proxy_serves_healthz_and_logs_the_bound_address() {
    let port = free_port();
    // Held for the whole test body: `KillOnDrop` reaps the child when the scope ends.
    let (_child, drained) = spawn_proxy(port);

    let body = wait_for_healthz(port);
    assert_eq!(body, "ok");
    wait_for_log(&drained, "listening");
}
