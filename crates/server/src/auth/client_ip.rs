//! Client IP for auth rate limits (peer address, or `X-Forwarded-For` behind loopback proxy).

use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;

pub fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    if let Some(peer) = peer {
        if peer.ip().is_loopback()
            && let Some(forwarded) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok())
        {
            if let Some(first) = forwarded.split(',').next() {
                let trimmed = first.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
        return peer.ip().to_string();
    }
    "unknown".to_string()
}

pub fn is_loopback_ip(ip: &str) -> bool {
    ip.parse::<IpAddr>()
        .map(|a| a.is_loopback())
        .unwrap_or(false)
}
