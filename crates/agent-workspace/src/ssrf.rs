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

/// Which non-public addresses a download may still use.
///
/// Production code can name only [`Reach::Public`]. `Reach::Loopback` exists
/// only in tests, so a loopback server can prove that a later hop is checked again.
#[derive(Clone, Copy)]
pub(crate) enum Reach {
    /// Refuse every non-public address.
    Public,
    /// Allow 127.0.0.0/8, `::1`, and IPv4-mapped forms of those.
    /// Every other non-public class stays blocked.
    #[cfg(test)]
    Loopback,
}

pub(crate) fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => blocked_v4(v4),
        IpAddr::V6(v6) => blocked_v6(v6),
    }
}

fn blocked_v4(ip: Ipv4Addr) -> bool {
    this_network(ip)
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || cgnat(ip)
}

/// `0.0.0.0/8`, "this network" — the whole block, not only `0.0.0.0`.
fn this_network(ip: Ipv4Addr) -> bool {
    ip.octets()[0] == 0
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
    ula(ip) || ip.is_unicast_link_local() || site_local(ip)
}

fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = mapped_or_compatible(ip) {
        return Some(v4);
    }
    nat64_or_6to4(ip)
}

fn mapped_or_compatible(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return Some(mapped);
    }
    compatible_v4(ip)
}

fn nat64_or_6to4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = nat64_v4(ip) {
        return Some(v4);
    }
    sixto4_v4(ip)
}

/// Well-known NAT64 prefix `64:ff9b::/96` (RFC 6052). The last 32 bits are an IPv4 address.
fn nat64_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let octets = ip.octets();
    if !octets.starts_with(&NAT64_WELL_KNOWN) {
        return None;
    }
    Some(Ipv4Addr::new(
        octets[12], octets[13], octets[14], octets[15],
    ))
}

const NAT64_WELL_KNOWN: [u8; 12] = [0x00, 0x64, 0xff, 0x9b, 0, 0, 0, 0, 0, 0, 0, 0];

/// 6to4 `2002::/16` (RFC 3056). The next 32 bits are an IPv4 address.
fn sixto4_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if ip.segments()[0] != 0x2002 {
        return None;
    }
    let octets = ip.octets();
    Some(Ipv4Addr::new(octets[2], octets[3], octets[4], octets[5]))
}

/// Deprecated site-local `fec0::/10` (RFC 3879).
fn site_local(ip: Ipv6Addr) -> bool {
    ip.segments()[0] & 0xffc0 == 0xfec0
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

pub(crate) fn vet_and_pin(pins: &Pins, url: &str, reach: Reach) -> Result<(), WorkspaceError> {
    let (host, ips) = vetted_host(url, reach)?;
    pins.pin(&host, &ips);
    Ok(())
}

fn vetted_host(url: &str, reach: Reach) -> Result<(String, Vec<IpAddr>), WorkspaceError> {
    let parsed = http_url(url)?;
    checked_host(&parsed, reach)
}

fn checked_host(
    parsed: &reqwest::Url,
    reach: Reach,
) -> Result<(String, Vec<IpAddr>), WorkspaceError> {
    let host = host_name(parsed)?;
    let ips = addresses(&host)?;
    refuse_if_blocked(&ips, reach)?;
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

pub(crate) fn refuse_if_blocked(ips: &[IpAddr], reach: Reach) -> Result<(), WorkspaceError> {
    if has_blocked(ips, reach) {
        return Err(WorkspaceError::BlockedAddress);
    }
    Ok(())
}

fn has_blocked(ips: &[IpAddr], reach: Reach) -> bool {
    for ip in ips {
        if blocked_for(*ip, reach) {
            return true;
        }
    }
    false
}

fn blocked_for(ip: IpAddr, reach: Reach) -> bool {
    match reach {
        Reach::Public => is_blocked_ip(ip),
        #[cfg(test)]
        Reach::Loopback => is_blocked_ip(ip) && !loopback_ip(ip),
    }
}

/// `127.0.0.0/8`, `::1`, and IPv4-mapped forms of those. Compatible `::a.b.c.d` is not included.
#[cfg(test)]
fn loopback_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback() || mapped_loopback(v6),
    }
}

#[cfg(test)]
fn mapped_loopback(ip: Ipv6Addr) -> bool {
    ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
}

pub(crate) fn client(
    pins: Arc<Pins>,
    reach: Reach,
) -> Result<reqwest::blocking::Client, WorkspaceError> {
    let resolver = Arc::clone(&pins);
    reqwest::blocking::Client::builder()
        .redirect(policy(pins, reach))
        .dns_resolver(resolver)
        .no_proxy()
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(WorkspaceError::io)
}

fn policy(pins: Arc<Pins>, reach: Reach) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| on_redirect(&pins, reach, attempt))
}

fn on_redirect(
    pins: &Pins,
    reach: Reach,
    attempt: reqwest::redirect::Attempt<'_>,
) -> reqwest::redirect::Action {
    if attempt.previous().len() >= MAX_REDIRECTS {
        return attempt.error(WorkspaceError::io("too many redirects"));
    }
    if let Err(err) = vet_and_pin(pins, attempt.url().as_str(), reach) {
        return attempt.error(err);
    }
    attempt.follow()
}
