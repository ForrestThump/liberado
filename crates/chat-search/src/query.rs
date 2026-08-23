//! Query parsing: literal multi-term AND mode (whitespace-split, `"quoted phrase"` support) and
//! regex mode.
//!
//! # Future extension
//! Literal mode's AND-only semantics are v1 scope, chosen for "vague recall of a topic" — a few
//! half-remembered keywords should narrow toward the right conversation, not flood the results
//! with an OR. Boolean-expression support (`term1 OR term2`, grouping) is a natural next step if
//! ever needed — the parsed representation could grow a `Vec<Vec<String>>` (outer = OR, inner =
//! AND) without touching the regex path at all.

use regex::{Regex, RegexBuilder};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum QueryParseError {
    #[error("invalid regex: {0}")]
    Regex(#[from] regex::Error),
    #[error("query is empty")]
    Empty,
}

/// A parsed, ready-to-match query.
#[derive(Debug, Clone)]
pub enum ParsedQuery {
    /// Literal mode: ALL terms must match (case-insensitive substring). Each term is already
    /// lowercased for matching.
    Literal(Vec<String>),
    /// Regex mode: the compiled pattern, case-insensitive.
    Regex(Regex),
}

impl ParsedQuery {
    /// Parse `raw` as a literal multi-term query. Splits on whitespace; `"quoted phrases"` count
    /// as one term. `Err(Empty)` when no terms result (blank/whitespace-only input).
    pub fn parse_literal(raw: &str) -> Result<Self, QueryParseError> {
        let terms = tokenize_literal(raw);
        if terms.is_empty() {
            return Err(QueryParseError::Empty);
        }
        Ok(ParsedQuery::Literal(terms))
    }

    /// Parse `raw` as a single regex pattern, matched case-insensitively.
    pub fn parse_regex(raw: &str) -> Result<Self, QueryParseError> {
        if raw.trim().is_empty() {
            return Err(QueryParseError::Empty);
        }
        let re = RegexBuilder::new(raw).case_insensitive(true).build()?;
        Ok(ParsedQuery::Regex(re))
    }

    /// Whether `haystack` matches this query.
    pub fn matches(&self, haystack: &str) -> bool {
        match self {
            ParsedQuery::Literal(terms) => {
                let lower = haystack.to_lowercase();
                terms.iter().all(|t| lower.contains(t.as_str()))
            }
            ParsedQuery::Regex(re) => re.is_match(haystack),
        }
    }

    /// The byte offset of the first match in `haystack`, if any — used to center a snippet.
    pub fn find_start(&self, haystack: &str) -> Option<usize> {
        match self {
            ParsedQuery::Literal(terms) => {
                let lower = haystack.to_lowercase();
                terms.iter().filter_map(|t| lower.find(t.as_str())).min()
            }
            ParsedQuery::Regex(re) => re.find(haystack).map(|m| m.start()),
        }
    }
}

/// Split `raw` on whitespace, treating a `"..."` run as a single token. Tokens are lowercased for
/// case-insensitive literal matching.
fn tokenize_literal(raw: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = raw.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if c == '"' {
            chars.next(); // consume opening quote
            let mut buf = String::new();
            for ch in chars.by_ref() {
                if ch == '"' {
                    break;
                }
                buf.push(ch);
            }
            if !buf.is_empty() {
                tokens.push(buf.to_lowercase());
            }
        } else {
            let mut buf = String::new();
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() {
                    break;
                }
                buf.push(ch);
                chars.next();
            }
            if !buf.is_empty() {
                tokens.push(buf.to_lowercase());
            }
        }
    }
    tokens
}

#[cfg(test)]
#[path = "query/tests.rs"]
mod survivor_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_splits_on_whitespace_and_lowercases() {
        let q = ParsedQuery::parse_literal("Hello WORLD").unwrap();
        match q {
            ParsedQuery::Literal(terms) => assert_eq!(terms, vec!["hello", "world"]),
            _ => panic!("expected Literal"),
        }
    }

    #[test]
    fn literal_quoted_phrase_is_one_term() {
        let q = ParsedQuery::parse_literal("\"quick brown\" fox").unwrap();
        match q {
            ParsedQuery::Literal(terms) => {
                assert_eq!(terms, vec!["quick brown", "fox"]);
            }
            _ => panic!("expected Literal"),
        }
    }

    #[test]
    fn literal_empty_query_is_err() {
        assert!(matches!(
            ParsedQuery::parse_literal("   "),
            Err(QueryParseError::Empty)
        ));
    }

    #[test]
    fn literal_and_requires_all_terms() {
        let q = ParsedQuery::parse_literal("hello goodbye").unwrap();
        assert!(!q.matches("hello world"));
        assert!(q.matches("hello and goodbye"));
    }

    #[test]
    fn literal_matches_case_insensitively() {
        let q = ParsedQuery::parse_literal("Tailscale").unwrap();
        assert!(q.matches("the tailscale firewall blocked it"));
    }

    #[test]
    fn regex_invalid_pattern_is_err() {
        assert!(ParsedQuery::parse_regex("(unclosed").is_err());
    }

    #[test]
    fn regex_matches_case_insensitively() {
        let q = ParsedQuery::parse_regex("error: .+refused").unwrap();
        assert!(q.matches("Error: connection refused"));
    }

    #[test]
    fn regex_empty_pattern_is_err() {
        assert!(matches!(
            ParsedQuery::parse_regex(""),
            Err(QueryParseError::Empty)
        ));
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn proptest_literal_matches_agrees_with_reference(
            terms in proptest::collection::vec("[a-z]{1,6}", 1..5),
            haystack in "[a-zA-Z ]{0,40}",
        ) {
            let input = terms.join(" ");
            let Ok(q) = ParsedQuery::parse_literal(&input) else {
                return Ok(());
            };
            let lower = haystack.to_lowercase();
            let expected = terms.iter().all(|t| lower.contains(&t.to_lowercase()));
            prop_assert_eq!(q.matches(&haystack), expected);
        }

        #[test]
        fn proptest_find_start_consistency(
            terms in proptest::collection::vec("[a-z]{2,6}", 1..4),
            haystack in "[a-zA-Z ]{0,80}",
        ) {
            let input = terms.join(" ");
            let Ok(q) = ParsedQuery::parse_literal(&input) else {
                return Ok(());
            };
            if q.matches(&haystack) {
                let start = q.find_start(&haystack);
                prop_assert!(start.is_some());
                prop_assert!(start.unwrap() <= haystack.len());
            }
        }

        #[test]
        fn proptest_regex_parse_no_panic(
            pattern in ".{0,20}",
        ) {
            let _ = ParsedQuery::parse_regex(&pattern);
        }

        #[test]
        fn proptest_literal_empty_rejected(
            space in "\\s+",
        ) {
            prop_assert!(ParsedQuery::parse_literal(&space).is_err());
        }
    }
}
