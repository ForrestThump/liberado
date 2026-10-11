//! Permanent localparts for agent puppets.
//!
//! A slug is lowercase `[a-z0-9_]` and is assigned once. The user id
//! `@liberado_<slug>:<server>` has to match the application-service namespace
//! `^@liberado(_[a-z0-9._=-]+)?:<server>$`. `pings` and `bridge` stay reserved.

use std::collections::HashSet;

const RESERVED: [&str; 2] = ["pings", "bridge"];
const MAX_SLUG: usize = 64;

/// Lowercase `[a-z0-9_]` form of a title or session id. Empty input becomes `agent`.
pub fn slug_base(source: &str) -> String {
    let mut out = String::new();
    let mut underscore = false;
    for ch in source.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            underscore = false;
            continue;
        }
        if !out.is_empty() && !underscore {
            out.push('_');
            underscore = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.len() > MAX_SLUG {
        out.truncate(MAX_SLUG);
        while out.ends_with('_') {
            out.pop();
        }
    }
    if out.is_empty() {
        "agent".to_string()
    } else {
        out
    }
}

/// Title when it has characters, otherwise the session id.
pub fn slug_source(title: &str, session_id: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        session_id.to_string()
    } else {
        title.to_string()
    }
}

/// `true` for the reserved localpart suffixes.
pub fn slug_reserved(slug: &str) -> bool {
    RESERVED.contains(&slug)
}

/// First free slug. Reserved words and `taken` are skipped. `base_2`, `base_3`, …
pub fn allocate_slug(base: &str, taken: &HashSet<String>) -> String {
    let base = slug_base(base);
    let mut n = 1u32;
    loop {
        let candidate = if n == 1 {
            base.clone()
        } else {
            numbered(&base, n)
        };
        if !slug_reserved(&candidate) && !taken.contains(&candidate) {
            return candidate;
        }
        n = n.saturating_add(1);
        if n == u32::MAX {
            return numbered(&base, n);
        }
    }
}

fn numbered(base: &str, n: u32) -> String {
    let suffix = format!("_{n}");
    let room = MAX_SLUG.saturating_sub(suffix.len()).max(1);
    let mut stem = slug_base(base);
    if stem.len() > room {
        stem.truncate(room);
        while stem.ends_with('_') {
            stem.pop();
        }
    }
    if stem.is_empty() {
        stem = "agent".to_string();
    }
    format!("{stem}{suffix}")
}

/// `@liberado_<slug>:<server>`.
pub fn puppet_mxid(slug: &str, server_name: &str) -> String {
    format!("@{}:{}", puppet_localpart(slug), server_name)
}

/// `liberado_<slug>`.
pub fn puppet_localpart(slug: &str) -> String {
    format!("liberado_{slug}")
}

/// Slug from a puppet user id, when it has the `liberado_` prefix on `server_name`.
pub fn slug_of_mxid(mxid: &str, server_name: &str) -> Option<String> {
    let rest = mxid.strip_prefix('@')?;
    let (localpart, host) = rest.rsplit_once(':')?;
    if !host.eq_ignore_ascii_case(server_name) {
        return None;
    }
    let slug = localpart.strip_prefix("liberado_")?;
    if slug.is_empty() || slug_reserved(slug) {
        return None;
    }
    Some(slug.to_string())
}

#[cfg(test)]
#[path = "slug_tests.rs"]
mod tests;
