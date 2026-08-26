//! Survivor tests for `catalog.rs`: prefix helpers, palette filtering, Tab
//! completion arithmetic, and the Telegram menu projection.

use super::*;

// ── common_prefix ───────────────────────────────────────────────────────────

/// Case-insensitive on the comparison, `a`'s casing in the result, byte-exact
/// on the cut (the trailing space of an insert is a real character).
#[test]
fn common_prefix_basics() {
    assert_eq!(common_prefix("/theme", "/THEME list"), "/theme");
    assert_eq!(common_prefix("abc", "xyz"), "");
    assert_eq!(common_prefix("/join x", "/join y"), "/join ");
    assert_eq!(common_prefix("", "/anything"), "");
}

// ── starts_with_ignore_ascii_case ───────────────────────────────────────────

#[test]
fn starts_with_ignore_ascii_case_matrix() {
    assert!(starts_with_ignore_ascii_case("abc", ""));
    assert!(starts_with_ignore_ascii_case("/theme list", "/theme"));
    assert!(starts_with_ignore_ascii_case("/HELP", "/help"));
    assert!(!starts_with_ignore_ascii_case("ab", "abc"));
    assert!(!starts_with_ignore_ascii_case("abd", "abc"));
    // An empty haystack rejects a non-empty prefix rather than panicking.
    assert!(!starts_with_ignore_ascii_case("", "a"));
}

/// The ghost suffix is the case-folded remainder: typing `/HEL` over `/help`
/// ghosts exactly `p`.
#[test]
fn ghost_suffix_folds_case_and_slices_by_chars() {
    assert_eq!(ghost_suffix("/HEL", 0).as_deref(), Some("p"));
    assert_eq!(ghost_suffix("/help", 0), None, "nothing left to ghost");
    assert_eq!(ghost_suffix("/nope", 0), None);
}

// ── slash detection and filtering ───────────────────────────────────────────

#[test]
fn slash_prefix_detection() {
    assert!(is_slash_prefix("/"));
    assert!(is_slash_prefix("/th"));
    assert!(is_slash_prefix("  /th"));
    assert!(!is_slash_prefix(""));
    assert!(!is_slash_prefix("hello"));
    assert!(!is_slash_prefix("say /hi"));
    // Only the first line decides.
    assert!(!is_slash_prefix("hello\n/world"));
}

/// Entries whose insert extends past their name still match by insert.
/// Typing the family space (`/theme `) outgrows the parent's display name,
/// so only its insert keeps the umbrella entry in the palette.
#[test]
fn filter_matches_by_insert_not_only_name() {
    assert!(
        filter_commands("/session ")
            .iter()
            .any(|spec| spec.name == "/session …"),
        "insert-only match lost (session): {:?}",
        filter_commands("/session ")
    );
    assert!(
        filter_commands("/theme ")
            .iter()
            .any(|spec| spec.name == "/theme"),
        "insert-only match lost (theme): {:?}",
        filter_commands("/theme ")
    );
}

// ── telegram menu ───────────────────────────────────────────────────────────

/// The advertised menu is exact and ordered; leading slashes are trimmed.
#[test]
fn telegram_menu_is_exact_and_ordered() {
    let expected = vec![
        ("help", "show this help"),
        ("new", "start a new conversation"),
        ("status", "show daemon connection info"),
        ("sessions", "session switcher (prior chats + goal sessions)"),
        (
            "spawn",
            "start an interactive session: /spawn <profile|domain> <goal>",
        ),
        (
            "goal",
            "coding goal: /goal <text> | in <project> <text> | status|pause|resume|clear",
        ),
        ("join", "join a goal session by id (focus its input)"),
        ("model", "browse eligible models (type to search)"),
        (
            "fork",
            "branch this conversation, keeping the original (/fork <turn> to go back)",
        ),
    ];
    assert_eq!(telegram_commands(), expected);
}

// ── tab completion ──────────────────────────────────────────────────────────

/// A family Tab-completes to the longest common insert prefix when that
/// prefix extends past what was typed: the theme family shares `/theme `
/// including its trailing space. An out-of-range selection reaches this
/// path; a valid one takes the selected-match shortcut instead.
#[test]
fn extending_family_completes_to_common_prefix() {
    assert_eq!(
        complete_commands("/the", 999).as_deref(),
        Some("/theme "),
        "theme family shares the trailing space"
    );
}

/// When the shared prefix adds nothing beyond what was typed — the `/s`
/// family (status/session/spawn) shares exactly `/s`, the whole query — Tab
/// jumps to the first match's full insert for a decisive completion.
#[test]
fn exhausted_prefix_jumps_to_first_insert() {
    assert_eq!(complete_commands("/", 999).as_deref(), Some("/help"));
    assert_eq!(complete_commands("/s", 999).as_deref(), Some("/status"));
    assert_eq!(
        complete_commands("/session", 999).as_deref(),
        Some("/session")
    );
}

/// A single match completes straight to its insert.
#[test]
fn single_match_completes_to_its_insert() {
    assert_eq!(complete_commands("/qui", 0).as_deref(), Some("/quit"));
}
