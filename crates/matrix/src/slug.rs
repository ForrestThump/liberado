//! Permanent localparts for agent puppets.
//!
//! A slug is lowercase `[a-z0-9_]` and is assigned once. The user id
//! `@liberado_<slug>:<server>` has to match the application-service namespace
//! `^@liberado(_[a-z0-9._=-]+)?:<server>$`. `pings` and `bridge` stay reserved.
//! New slugs are at most 24 characters, cut on a word boundary when the title
//! has one. A row that already has a slug keeps it.

use std::collections::HashSet;

const RESERVED: [&str; 2] = ["pings", "bridge"];
/// Cap for a new slug, including a `_2` dedupe suffix.
const MAX_SLUG: usize = 24;
/// Tail of a session id used when the title does not yield a slug.
const ID_SLUG: usize = 12;

/// Lowercase `[a-z0-9_]` form of a title or session id. Empty input becomes `agent`.
pub fn slug_base(source: &str) -> String {
    let capped = cap_words(&slug_body(strip_ellipsis(source.trim())), MAX_SLUG);
    if capped.is_empty() {
        "agent".to_string()
    } else {
        capped
    }
}

/// Cleaned title, or a short slug taken from the session id when the title is empty
/// or has no letters or digits.
pub fn slug_source(title: &str, session_id: &str) -> String {
    let cleaned = strip_ellipsis(title.trim());
    if slug_body(cleaned).is_empty() {
        short_id(session_id)
    } else {
        cleaned.to_string()
    }
}

fn strip_ellipsis(input: &str) -> &str {
    let mut text = input.trim();
    loop {
        let next = text.trim_end_matches(['…', '⋯']).trim_end();
        if let Some(stripped) = next.strip_suffix("...") {
            text = stripped.trim_end();
            continue;
        }
        return next;
    }
}

fn slug_body(source: &str) -> String {
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
    out
}

/// Keep at most `max` characters, ending on an underscore boundary when one fits.
fn cap_words(slug: &str, max: usize) -> String {
    if slug.len() <= max {
        return slug.trim_end_matches('_').to_string();
    }
    let window = &slug[..max];
    let cut = match window.rfind('_') {
        Some(0) | None => window,
        Some(idx) => &window[..idx],
    };
    let trimmed = cut.trim_end_matches('_');
    if trimmed.is_empty() {
        window.trim_end_matches('_').to_string()
    } else {
        trimmed.to_string()
    }
}

fn short_id(session_id: &str) -> String {
    let body = slug_body(session_id.trim());
    if body.is_empty() {
        return "agent".to_string();
    }
    if body.len() <= ID_SLUG {
        return body;
    }
    let start = body.len() - ID_SLUG;
    body[start..].to_string()
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
    let stem = cap_words(base, room);
    let stem = if stem.is_empty() { "a" } else { &stem };
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
