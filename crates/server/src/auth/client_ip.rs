//! Client IP for auth rate limits (peer address, or `X-Forwarded-For` behind loopback proxy).

use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;

pub fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    if let Some(peer) = peer {
        if peer.ip().is_loopback()
            && let Some(forwarded) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok())
            && let Some(first) = forwarded.split(',').next()
        {
            let trimmed = first.trim();
            if !trimmed.is_empty() && !is_loopback_ip(trimmed) {
                return trimmed.to_string();
            }
        }
        return peer.ip().to_string();
    }
    "unknown".to_string()
}

pub fn is_loopback_ip(ip: &str) -> bool {
    ip.parse::<IpAddr>().is_ok_and(|a| a.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_ip_detection_matches_parse() {
        assert!(is_loopback_ip("127.0.0.1"));
        assert!(!is_loopback_ip("203.0.113.1"));
    }
}
