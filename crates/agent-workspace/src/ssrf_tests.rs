//! Strict downloads refuse non-public addresses. Happy-path HTTP tests use the test-only client.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};

use crate::WorkspaceError;
use crate::quota::ENTRY_COST;
use crate::ssrf::{self, is_blocked_ip};
use crate::tools::downloaded_message;
use crate::workspace::AgentWorkspace;

fn open(root: &Path) -> AgentWorkspace {
    AgentWorkspace::open(root, "ssrf", ENTRY_COST * 8).unwrap()
}

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn ip(text: &str) -> IpAddr {
    text.parse().expect(text)
}

#[test]
fn blocked_address_classes_are_refused() {
    for text in [
        "0.0.0.0",
        "127.0.0.1",
        "127.255.255.255",
        "10.1.2.3",
        "172.16.0.1",
        "172.31.255.255",
        "192.168.1.1",
        "169.254.169.254",
        "100.64.0.1",
        "100.127.255.255",
        "224.0.0.1",
        "255.255.255.255",
        "::",
        "::1",
        "fc00::1",
        "fd12::1",
        "fe80::1",
        "ff02::1",
        "::ffff:127.0.0.1",
        "::ffff:10.1.2.3",
        "::ffff:169.254.169.254",
        "::ffff:100.64.0.1",
        "::ffff:255.255.255.255",
        "::7f00:1",
    ] {
        assert!(is_blocked_ip(ip(text)), "{text}");
    }
}

#[test]
fn public_and_documentation_addresses_are_allowed() {
    for text in [
        "8.8.8.8",
        "1.1.1.1",
        "11.0.0.1",
        "100.63.255.255",
        "100.128.0.1",
        "172.15.255.255",
        "172.32.0.1",
        "192.0.2.10",
        "192.0.2.1",
        "fec0::1",
        "2001:db8::1",
        "::ffff:8.8.8.8",
        "::ffff:192.0.2.10",
    ] {
        assert!(!is_blocked_ip(ip(text)), "{text}");
    }
}

#[test]
fn one_private_address_blocks_the_whole_answer() {
    let mixed = [ip("8.8.8.8"), ip("10.0.0.1")];
    let err = ssrf::refuse_if_blocked(&mixed, false).unwrap_err();
    assert!(matches!(err, WorkspaceError::BlockedAddress));
    assert!(ssrf::refuse_if_blocked(&mixed, true).is_ok());
    assert!(ssrf::refuse_if_blocked(&[ip("192.0.2.10")], false).is_ok());
}

#[test]
fn strict_download_refuses_non_public_targets() {
    let root = scratch();
    let workspace = open(root.path());
    for url in [
        "http://127.0.0.1:9/x",
        "http://10.1.2.3/x",
        "http://172.16.0.1/x",
        "http://192.168.1.1/x",
        "http://169.254.169.254/latest/meta-data",
        "http://100.64.0.1/x",
        "http://0.0.0.0/x",
        "http://224.0.0.1/x",
        "http://255.255.255.255/x",
        "http://[::1]:9/x",
        "http://[::]/x",
        "http://[fc00::1]/x",
        "http://[fe80::1]/x",
        "http://[::ffff:10.1.2.3]/x",
        "http://[::ffff:127.0.0.1]/x",
        "http://[::7f00:1]/x",
        "http://localhost/x",
    ] {
        let err = workspace.download_url("a.txt", url).unwrap_err();
        assert!(
            matches!(err, WorkspaceError::BlockedAddress),
            "{url} returned {err:?}"
        );
    }
    assert!(!workspace.files_dir().join("a.txt").exists());
}

#[tokio::test]
async fn the_resolver_never_falls_through_to_dns() {
    let pins = ssrf::Pins::new();
    let err = resolve(&pins, "localhost").await.unwrap_err();
    assert!(err.to_string().contains("not pinned"), "{err}");

    ssrf::vet_and_pin(&pins, "http://192.0.2.10/a", false).unwrap();
    let addrs = resolve(&pins, "192.0.2.10").await.unwrap();
    assert_eq!(addrs, vec![SocketAddr::from(([192, 0, 2, 10], 0))]);

    let err = ssrf::vet_and_pin(&pins, "http://10.1.2.3/a", false).unwrap_err();
    assert!(matches!(err, WorkspaceError::BlockedAddress));
    assert!(resolve(&pins, "10.1.2.3").await.is_err());

    ssrf::vet_and_pin(&pins, "http://127.0.0.1/a", true).unwrap();
    let pinned = resolve(&pins, "127.0.0.1").await.unwrap();
    assert_eq!(pinned[0].ip(), ip("127.0.0.1"));
}

#[tokio::test]
async fn localhost_is_blocked_after_name_resolution() {
    let pins = ssrf::Pins::new();
    let err = ssrf::vet_and_pin(&pins, "http://localhost/secret", false).unwrap_err();
    assert!(matches!(err, WorkspaceError::BlockedAddress), "{err:?}");
    assert!(resolve(&pins, "localhost").await.is_err());

    ssrf::vet_and_pin(&pins, "http://LocalHost/secret", true).unwrap();
    let addrs = resolve(&pins, "localhost").await.unwrap();
    assert!(addrs.iter().any(|addr| addr.ip().is_loopback()));
}

#[test]
fn allowing_local_download_reports_the_stored_bytes() {
    let root = scratch();
    let cap = ENTRY_COST + 100;
    let workspace = AgentWorkspace::open(root.path(), "ssrf", cap).unwrap();
    let url = local_body(b"hello");
    let bytes = workspace
        .download_url_allowing_local("a.txt", &url)
        .unwrap();
    let used = workspace.usage_bytes().unwrap();
    let text = downloaded_message(bytes, "a.txt", used, cap);
    assert_eq!(
        text,
        format!("Downloaded 5 bytes to a.txt. Using {used} of {cap} bytes.")
    );
    assert_eq!(workspace.read_text("a.txt").unwrap(), "hello");
}

#[cfg(unix)]
#[test]
fn strict_download_allows_a_documentation_address() {
    let _lease = DocLease::acquire();
    let root = scratch();
    let workspace = open(root.path());
    let url = serve_doc(&ok_page(b"ok"));
    let bytes = workspace.download_url("a.txt", &url).unwrap();
    assert_eq!(bytes, 2);
    assert_eq!(workspace.read_text("a.txt").unwrap(), "ok");
}

#[cfg(unix)]
#[test]
fn strict_download_refuses_a_redirect_to_a_private_address() {
    let _lease = DocLease::acquire();
    let root = scratch();
    let workspace = open(root.path());
    for location in [
        "http://10.1.2.3/secret",
        "http://127.0.0.1:1/secret",
        "http://localhost:1/secret",
        "http://169.254.169.254/latest/meta-data",
    ] {
        let url = serve_doc(&redirect_page(location));
        let err = workspace.download_url("a.txt", &url).unwrap_err();
        assert!(
            matches!(err, WorkspaceError::BlockedAddress),
            "{location} -> {err:?}"
        );
        assert!(!workspace.files_dir().join("a.txt").exists());
        assert!(!workspace.home_dir().join("incoming.bin").exists());
    }
}

async fn resolve(
    pins: &ssrf::Pins,
    host: &str,
) -> Result<Vec<SocketAddr>, Box<dyn std::error::Error + Send + Sync>> {
    use reqwest::dns::{Name, Resolve};
    use std::str::FromStr;
    let iter = pins.resolve(Name::from_str(host).unwrap()).await?;
    Ok(iter.collect())
}

fn local_body(body: &[u8]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let page = ok_page(body);
    std::thread::spawn(move || write_once(listener, &page));
    format!("http://{addr}/blob")
}

#[cfg(unix)]
fn serve_doc(page: &[u8]) -> String {
    let listener = TcpListener::bind("192.0.2.10:0").expect("bind 192.0.2.10");
    let addr = listener.local_addr().unwrap();
    let page = page.to_vec();
    std::thread::spawn(move || write_once(listener, &page));
    format!("http://{addr}/blob")
}

fn write_once(listener: TcpListener, page: &[u8]) {
    let Ok((mut sock, _)) = listener.accept() else {
        return;
    };
    let mut buf = [0u8; 2048];
    let _ = Read::read(&mut sock, &mut buf);
    let _ = Write::write_all(&mut sock, page);
}

fn ok_page(body: &[u8]) -> Vec<u8> {
    let mut page = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    page.extend_from_slice(body);
    page
}

#[cfg(unix)]
fn redirect_page(location: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .into_bytes()
}

#[cfg(unix)]
static DOC: Mutex<()> = Mutex::new(());

#[cfg(unix)]
struct DocLease {
    _lock: MutexGuard<'static, ()>,
    added: bool,
}

#[cfg(unix)]
impl DocLease {
    fn acquire() -> Self {
        let lock = DOC.lock().unwrap_or_else(|err| err.into_inner());
        if doc_ip_present() {
            return Self {
                _lock: lock,
                added: false,
            };
        }
        let lease = Self {
            _lock: lock,
            added: true,
        };
        let ok = run_ip(false, "add") || run_ip(true, "add");
        assert!(ok && doc_ip_present(), "could not add 192.0.2.10/32 to lo");
        lease
    }
}

#[cfg(unix)]
impl Drop for DocLease {
    fn drop(&mut self) {
        if self.added {
            let _ = run_ip(false, "del") || run_ip(true, "del");
        }
    }
}

#[cfg(unix)]
fn doc_ip_present() -> bool {
    let Ok(output) = Command::new("ip")
        .args(["-4", "addr", "show", "dev", "lo"])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout).contains("192.0.2.10")
}

#[cfg(unix)]
fn run_ip(sudo: bool, verb: &str) -> bool {
    let mut cmd = if sudo {
        let mut cmd = Command::new("sudo");
        cmd.arg("-n").arg("ip");
        cmd
    } else {
        Command::new("ip")
    };
    cmd.args(["addr", verb, "192.0.2.10/32", "dev", "lo"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    matches!(cmd.status(), Ok(status) if status.success())
}
