use std::collections::HashSet;

use super::{allocate_slug, puppet_localpart, puppet_mxid, slug_base, slug_of_mxid, slug_source};
use crate::filter::in_user_namespace;

const SERVER: &str = "matrix.homelab.local";

fn taken(slugs: &[&str]) -> HashSet<String> {
    slugs.iter().map(|slug| (*slug).to_string()).collect()
}

/// The anchored registration regex, written out so this does not call the production matcher.
fn registration_regex(mxid: &str) -> bool {
    let Some(rest) = mxid.strip_prefix("@liberado") else {
        return false;
    };
    let Some((suffix, host)) = rest.rsplit_once(':') else {
        return false;
    };
    if host != SERVER {
        return false;
    }
    if suffix.is_empty() {
        return true;
    }
    let Some(body) = suffix.strip_prefix('_') else {
        return false;
    };
    !body.is_empty()
        && body
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'=' | b'-'))
}

#[test]
fn titles_become_lowercase_slugs() {
    assert_eq!(slug_base("Coder"), "coder");
    assert_eq!(slug_base("  Coding Agent! "), "coding_agent");
    assert_eq!(slug_base("a--b"), "a_b");
    assert_eq!(slug_base("@@@"), "agent");
    assert_eq!(slug_source("  ", "01ABC"), "01abc");
    assert_eq!(slug_source("Coder", "01ABC"), "Coder");
    assert_eq!(slug_source("@@@", "01HZABCDEF"), "01hzabcdef");
    assert_eq!(slug_source("…", "01HZABCDEF"), "01hzabcdef");
}

#[test]
fn long_titles_cut_on_a_word_and_drop_a_trailing_ellipsis() {
    let title = "Create a vault note at conformance/approval-test.md conta…";
    let slug = slug_base(title);
    assert_eq!(slug, "create_a_vault_note_at");
    assert!(slug.len() <= 24);
    assert!(!slug.ends_with('_'));
    assert_eq!(slug_base(&"a".repeat(40)), "a".repeat(24));
    assert_eq!(slug_base("Personal task…"), "personal_task");
    assert_eq!(slug_base("Hello world..."), "hello_world");
    assert_eq!(
        allocate_slug(title, &HashSet::new()),
        "create_a_vault_note_at"
    );
    let id = "01JABCDEFGHJKMNPQRSTVWXYZ0";
    assert_eq!(slug_source("...", id), "npqrstvwxyz0");
    assert_eq!(slug_source("", id).len(), 12);
}

#[test]
fn dedupe_suffix_stays_inside_the_cap() {
    let base = "word_word_word_word_word";
    assert_eq!(slug_base(base).len(), 24);
    let slug = allocate_slug(base, &taken(&[base]));
    assert_eq!(slug, "word_word_word_word_2");
    assert!(slug.len() <= 24, "{slug}");
    assert!(slug.ends_with("_2"));
    assert!(!slug.ends_with('_'));
    let long = allocate_slug(
        "Create a vault note at conformance/approval-test.md conta…",
        &taken(&["create_a_vault_note_at"]),
    );
    assert_eq!(long, "create_a_vault_note_at_2");
    assert!(long.len() <= 24);
}

#[test]
fn reserved_and_colliding_slugs_are_numbered() {
    assert_eq!(allocate_slug("bridge", &HashSet::new()), "bridge_2");
    assert_eq!(allocate_slug("Pings", &HashSet::new()), "pings_2");
    assert_eq!(allocate_slug("Coder", &taken(&["coder"])), "coder_2");
    assert_eq!(
        allocate_slug("coder!", &taken(&["coder", "coder_2"])),
        "coder_3"
    );
    let first = allocate_slug("Coder", &HashSet::new());
    let second = allocate_slug("coder", &taken(&[&first]));
    assert_eq!(first, "coder");
    assert_eq!(second, "coder_2");
}

#[test]
fn allocated_user_ids_match_the_namespace() {
    for source in [
        "Coder",
        "bridge",
        "pings",
        "Research Bot",
        "01HZABCDEF",
        "Create a vault note at conformance/approval-test.md conta…",
        "word_word_word_word_word",
    ] {
        let slug = allocate_slug(source, &HashSet::new());
        let mxid = puppet_mxid(&slug, SERVER);
        assert_eq!(puppet_localpart(&slug), format!("liberado_{slug}"));
        assert!(
            registration_regex(&mxid),
            "{mxid} is outside ^@liberado(_[a-z0-9._=-]+)?:{SERVER}$"
        );
        assert!(in_user_namespace(&mxid, SERVER));
        assert_eq!(slug_of_mxid(&mxid, SERVER).as_deref(), Some(slug.as_str()));
    }
    assert!(slug_of_mxid("@liberado:matrix.homelab.local", SERVER).is_none());
    assert!(slug_of_mxid("@liberado_bridge:matrix.homelab.local", SERVER).is_none());
    assert!(slug_of_mxid("@liberado_pings:matrix.homelab.local", SERVER).is_none());
}
