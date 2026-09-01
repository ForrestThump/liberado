//! Magnitude classification helpers.
//!
//! Lives beside [`crate::capability`] so that crate can keep `assess_magnitude` as a
//! two-way branch while the sweeping-token scan stays in one place.

use crate::capability::Magnitude;

/// Whole-word quantifiers that always make an action sweeping.
///
/// `any` / `each` are intentionally omitted: they appear constantly in benign English
/// ("any details", "each field") and, combined with false-positive destructive stems, over-gated
/// read goals. `entire` is handled separately: it can qualify either a collection ("the entire
/// inbox") or one bounded object ("the entire line").
pub(crate) const SWEEPING_WORDS: &[&str] = &["all", "every", "everything"];

/// Collective objects for which "the entire …" genuinely means a bulk target.
///
/// A singular concrete object such as a line, task, note, or file is deliberately absent:
/// deleting that whole object is destructive, but its magnitude is still bounded.
const ENTIRE_SWEEPING_TARGETS: &[&str] = &[
    "account",
    "archive",
    "collection",
    "database",
    "directory",
    "folder",
    "inbox",
    "repository",
    "vault",
    "workspace",
];

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
}

/// True when `text` contains a universal quantifier, or `entire` qualifying a known collection.
pub(crate) fn is_sweeping(text: &str) -> bool {
    let tokens: Vec<String> = words(text).collect();
    tokens.iter().any(|w| SWEEPING_WORDS.contains(&w.as_str()))
        || tokens.windows(2).any(|window| {
            window[0] == "entire" && ENTIRE_SWEEPING_TARGETS.contains(&window[1].as_str())
        })
}

/// Classify how far-reaching `text` is. Public through [`crate::assess_magnitude`].
pub(crate) fn classify(text: &str) -> Magnitude {
    if is_sweeping(text) {
        Magnitude::Sweeping
    } else {
        Magnitude::Bounded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::is_sweeping_destructive;

    #[test]
    fn entire_line_stays_bounded() {
        assert!(!is_sweeping_destructive(
            "remove the entire line containing Mom's September birthday gift"
        ));
        assert!(!is_sweeping_destructive(
            "delete this task with the entire line removed"
        ));
        assert_eq!(
            classify("remove the entire line containing Mom's September birthday gift"),
            Magnitude::Bounded
        );
        assert_eq!(classify("clear the entire inbox"), Magnitude::Sweeping);
    }
}
