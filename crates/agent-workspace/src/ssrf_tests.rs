//! Strict downloads refuse non-public addresses. Happy-path HTTP tests use the test-only client,
//! which allows loopback and still refuses every other non-public class.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::path::Path;

use crate::WorkspaceError;
use crate::quota::ENTRY_COST;
use crate::ssrf::{self, Reach, is_blocked_ip};
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
        "0.1.2.3",
        "0.255.255.255",
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
        "fec0::1",
        "feff::1",
        "fe80::1",
        "ff02::1",
        "::ffff:0.1.2.3",
        "::ffff:127.0.0.1",
        "::ffff:10.1.2.3",
        "::ffff:169.254.169.254",
        "::ffff:100.64.0.1",
        "::ffff:255.255.255.255",
        "::7f00:1",
        "64:ff9b::",
        "64:ff9b::7f00:1",
        "64:ff9b::a00:1",
        "2002::",
        "2002:7f00:1::",
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
        "2001:db8::1",
        "::ffff:8.8.8.8",
        "::ffff:192.0.2.10",
        "64:ff9b::808:808",
        "2002:808:808::",
    ] {
        assert!(!is_blocked_ip(ip(text)), "{text}");
    }
}

#[test]
fn one_private_address_blocks_the_whole_answer() {
    let mixed = [ip("8.8.8.8"), ip("10.0.0.1")];
    let err = ssrf::refuse_if_blocked(&mixed, Reach::Public).unwrap_err();
    assert!(matches!(err, WorkspaceError::BlockedAddress));
    let err = ssrf::refuse_if_blocked(&mixed, Reach::Loopback).unwrap_err();
    assert!(matches!(err, WorkspaceError::BlockedAddress));
    assert!(ssrf::refuse_if_blocked(&[ip("192.0.2.10")], Reach::Public).is_ok());
    assert!(ssrf::refuse_if_blocked(&[ip("8.8.8.8")], Reach::Loopback).is_ok());
}

#[test]
fn loopback_reach_allows_only_loopback() {
    for text in [
        "127.0.0.1",
        "127.255.255.255",
        "::1",
        "::ffff:127.0.0.1",
        "::ffff:127.255.255.255",
    ] {
        assert!(
            ssrf::refuse_if_blocked(&[ip(text)], Reach::Loopback).is_ok(),
            "{text}"
        );
    }
    for text in [
        "10.1.2.3",
        "172.16.0.1",
        "192.168.1.1",
        "169.254.169.254",
        "100.64.0.1",
        "0.0.0.0",
        "0.1.2.3",
        "224.0.0.1",
        "255.255.255.255",
        "::",
        "fc00::1",
        "fe80::1",
        "fec0::1",
        "ff02::1",
        "::7f00:1",
        "::ffff:10.1.2.3",
        "::ffff:0.1.2.3",
        "64:ff9b::7f00:1",
        "2002:7f00:1::",
    ] {
        let err = ssrf::refuse_if_blocked(&[ip(text)], Reach::Loopback).unwrap_err();
        assert!(
            matches!(err, WorkspaceError::BlockedAddress),
            "{text} -> {err:?}"
        );
    }
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
        "http://0.1.2.3/x",
        "http://224.0.0.1/x",
        "http://255.255.255.255/x",
        "http://[::1]:9/x",
        "http://[::]/x",
        "http://[fc00::1]/x",
        "http://[fec0::1]/x",
        "http://[fe80::1]/x",
        "http://[::ffff:10.1.2.3]/x",
        "http://[::ffff:127.0.0.1]/x",
        "http://[::ffff:0.1.2.3]/x",
        "http://[::7f00:1]/x",
        "http://[64:ff9b::7f00:1]/x",
        "http://[2002:7f00:1::]/x",
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

    ssrf::vet_and_pin(&pins, "http://192.0.2.10/a", Reach::Public).unwrap();
    let addrs = resolve(&pins, "192.0.2.10").await.unwrap();
    assert_eq!(addrs, vec![SocketAddr::from(([192, 0, 2, 10], 0))]);

    let err = ssrf::vet_and_pin(&pins, "http://10.1.2.3/a", Reach::Public).unwrap_err();
    assert!(matches!(err, WorkspaceError::BlockedAddress));
    assert!(resolve(&pins, "10.1.2.3").await.is_err());

    ssrf::vet_and_pin(&pins, "http://127.0.0.1/a", Reach::Loopback).unwrap();
    let pinned = resolve(&pins, "127.0.0.1").await.unwrap();
    assert_eq!(pinned[0].ip(), ip("127.0.0.1"));
}

#[tokio::test]
async fn localhost_is_blocked_after_name_resolution() {
    let pins = ssrf::Pins::new();
    let err = ssrf::vet_and_pin(&pins, "http://localhost/secret", Reach::Public).unwrap_err();
    assert!(matches!(err, WorkspaceError::BlockedAddress), "{err:?}");
    assert!(resolve(&pins, "localhost").await.is_err());

    ssrf::vet_and_pin(&pins, "http://LocalHost/secret", Reach::Loopback).unwrap();
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

#[test]
fn strict_download_refuses_a_redirect_to_a_private_address() {
    let root = scratch();
    let workspace = open(root.path());
    for location in [
        "http://10.1.2.3/secret",
        "http://192.168.1.1/secret",
        "http://169.254.169.254/latest/meta-data",
        "http://[fc00::1]/secret",
        "http://100.64.0.1/secret",
    ] {
        let url = local_redirect(location);
        let err = workspace
            .download_url_allowing_local("a.txt", &url)
            .unwrap_err();
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
    serve_local(&ok_page(body))
}

fn local_redirect(location: &str) -> String {
    serve_local(&redirect_page(location))
}

fn serve_local(page: &[u8]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
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

fn redirect_page(location: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .into_bytes()
}
