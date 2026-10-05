//! Block downloads whose hosts resolve to a non-public address.
//!
//! The check resolves DNS first, then classifies every address. The TCP connection is pinned to
//! those addresses: the client resolver never calls the system resolver, so a later DNS answer
//! cannot swap in a blocked address. Each redirect is resolved and classified again.

use std::collections::HashMap;
use std::future::ready;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

use crate::error::WorkspaceError;

const MAX_REDIRECTS: usize = 5;

pub(crate) fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => blocked_v4(v4),
        IpAddr::V6(v6) => blocked_v6(v6),
    }
}

fn blocked_v4(ip: Ipv4Addr) -> bool {
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || cgnat(ip)
}

fn cgnat(ip: Ipv4Addr) -> bool {
    let [first, second, _, _] = ip.octets();
    first == 100 && second & 0xc0 == 0x40
}

fn blocked_v6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return true;
    }
    if let Some(v4) = embedded_v4(ip) {
        return blocked_v4(v4);
    }
    ula(ip) || ip.is_unicast_link_local()
}

fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return Some(mapped);
    }
    compatible_v4(ip)
}

/// IPv4-compatible `::a.b.c.d`. `::` and `::1` also answer `to_ipv4`, so they are excluded here.
fn compatible_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if ip.is_loopback() || ip.is_unspecified() {
        return None;
    }
    ip.to_ipv4()
}

fn ula(ip: Ipv6Addr) -> bool {
    ip.segments()[0] & 0xfe00 == 0xfc00
}

pub(crate) struct Pins {
    hosts: Mutex<HashMap<String, Vec<SocketAddr>>>,
}

impl Pins {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            hosts: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn pin(&self, host: &str, ips: &[IpAddr]) {
        self.map().insert(host.to_ascii_lowercase(), sockets(ips));
    }

    fn map(&self) -> MutexGuard<'_, HashMap<String, Vec<SocketAddr>>> {
        match self.hosts.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn pinned(&self, name: &str) -> Result<Addrs, Box<dyn std::error::Error + Send + Sync>> {
        match self.map().get(&name.to_ascii_lowercase()).cloned() {
            Some(addrs) => Ok(Box::new(addrs.into_iter())),
            None => Err(Box::new(std::io::Error::other(
                "host was not pinned; refusing a live DNS lookup",
            ))),
        }
    }
}

fn sockets(ips: &[IpAddr]) -> Vec<SocketAddr> {
    let mut addrs = Vec::with_capacity(ips.len());
    for ip in ips {
        addrs.push(SocketAddr::new(*ip, 0));
    }
    addrs
}

impl Resolve for Pins {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(ready(self.pinned(name.as_str())))
    }
}

pub(crate) fn vet_and_pin(pins: &Pins, url: &str, allow_local: bool) -> Result<(), WorkspaceError> {
    let (host, ips) = vetted_host(url, allow_local)?;
    pins.pin(&host, &ips);
    Ok(())
}

fn vetted_host(url: &str, allow_local: bool) -> Result<(String, Vec<IpAddr>), WorkspaceError> {
    let parsed = http_url(url)?;
    checked_host(&parsed, allow_local)
}

fn checked_host(
    parsed: &reqwest::Url,
    allow_local: bool,
) -> Result<(String, Vec<IpAddr>), WorkspaceError> {
    let host = host_name(parsed)?;
    let ips = addresses(&host)?;
    refuse_if_blocked(&ips, allow_local)?;
    Ok((host, ips))
}

fn http_url(url: &str) -> Result<reqwest::Url, WorkspaceError> {
    let parsed = reqwest::Url::parse(url).map_err(|_| WorkspaceError::UnsupportedUrl)?;
    if parsed.scheme() == "http" || parsed.scheme() == "https" {
        return Ok(parsed);
    }
    Err(WorkspaceError::UnsupportedUrl)
}

fn host_name(url: &reqwest::Url) -> Result<String, WorkspaceError> {
    match url.host_str() {
        Some(host) => Ok(host.to_ascii_lowercase()),
        None => Err(WorkspaceError::UnsupportedUrl),
    }
}

fn addresses(host: &str) -> Result<Vec<IpAddr>, WorkspaceError> {
    if let Some(ip) = literal_ip(host) {
        return Ok(vec![ip]);
    }
    if host.starts_with('[') {
        return Err(WorkspaceError::UnsupportedUrl);
    }
    lookup(host)
}

/// `host_str` keeps the brackets around an IPv6 address. `IpAddr` does not.
fn literal_ip(host: &str) -> Option<IpAddr> {
    let bare = match host.strip_prefix('[') {
        Some(rest) => rest.strip_suffix(']')?,
        None => host,
    };
    bare.parse().ok()
}

fn lookup(host: &str) -> Result<Vec<IpAddr>, WorkspaceError> {
    let found = (host, 0u16).to_socket_addrs().map_err(WorkspaceError::io)?;
    let ips: Vec<IpAddr> = found.map(|addr| addr.ip()).collect();
    if ips.is_empty() {
        return Err(WorkspaceError::io(format!("no address for {host}")));
    }
    Ok(ips)
}

pub(crate) fn refuse_if_blocked(ips: &[IpAddr], allow_local: bool) -> Result<(), WorkspaceError> {
    if allow_local {
        return Ok(());
    }
    if has_blocked(ips) {
        return Err(WorkspaceError::BlockedAddress);
    }
    Ok(())
}

fn has_blocked(ips: &[IpAddr]) -> bool {
    for ip in ips {
        if is_blocked_ip(*ip) {
            return true;
        }
    }
    false
}

pub(crate) fn client(
    pins: Arc<Pins>,
    allow_local: bool,
) -> Result<reqwest::blocking::Client, WorkspaceError> {
    let resolver = Arc::clone(&pins);
    reqwest::blocking::Client::builder()
        .redirect(policy(pins, allow_local))
        .dns_resolver(resolver)
        .no_proxy()
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(WorkspaceError::io)
}

fn policy(pins: Arc<Pins>, allow_local: bool) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| on_redirect(&pins, allow_local, attempt))
}

fn on_redirect(
    pins: &Pins,
    allow_local: bool,
    attempt: reqwest::redirect::Attempt<'_>,
) -> reqwest::redirect::Action {
    if attempt.previous().len() >= MAX_REDIRECTS {
        return attempt.error(WorkspaceError::io("too many redirects"));
    }
    if let Err(err) = vet_and_pin(pins, attempt.url().as_str(), allow_local) {
        return attempt.error(err);
    }
    attempt.follow()
}
