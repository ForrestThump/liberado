//! Shared slash-command catalog for progressive autocomplete / palettes.
//!
//! Lives here (not in TUI) so WebUI and CLI can reuse the same names, descriptions,
//! and filter semantics.

/// One completable slash entry shown in a command palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSpec {
    /// Text written into the input on Tab/select (may end with a trailing space for subcommands).
    pub insert: &'static str,
    /// Display name in the palette (usually without trailing space).
    pub name: &'static str,
    /// One-line help shown beside the name.
    pub description: &'static str,
}

/// Full static catalog. Order is display order when the user types `/`.
/// Safer commands first so bare `/` + Enter (ghost-accept) is not destructive.
pub const COMMAND_CATALOG: &[CommandSpec] = &[
    CommandSpec {
        insert: "/help",
        name: "/help",
        description: "show this help",
    },
    CommandSpec {
        insert: "/new",
        name: "/new",
        description: "start a new conversation",
    },
    CommandSpec {
        insert: "/clear",
        name: "/clear",
        description: "clear the chat display (local only)",
    },
    CommandSpec {
        insert: "/status",
        name: "/status",
        description: "show daemon connection info",
    },
    CommandSpec {
        insert: "/model",
        name: "/model",
        description: "browse eligible models (type to search)",
    },
    CommandSpec {
        insert: "/fork",
        name: "/fork",
        description: "fork current conversation",
    },
    CommandSpec {
        insert: "/theme ",
        name: "/theme",
        description: "theme switching (list, set, reload)",
    },
    CommandSpec {
        insert: "/theme list",
        name: "/theme list",
        description: "list available themes",
    },
    CommandSpec {
        insert: "/theme set ",
        name: "/theme set",
        description: "set theme by name",
    },
    CommandSpec {
        insert: "/theme reload",
        name: "/theme reload",
        description: "reload user themes from disk",
    },
    // `/session` (singular) precedes `/sessions` so the ambiguous `/session` prefix ghost-completes
    // to the conversation browser; typing the full `/sessions` filters uniquely to the switcher.
    CommandSpec {
        insert: "/session",
        name: "/session",
        description: "conversation browser (prior chats)",
    },
    CommandSpec {
        insert: "/sessions",
        name: "/sessions",
        description: "switch sessions (primary chat + goal sessions)",
    },
    CommandSpec {
        insert: "/spawn ",
        name: "/spawn",
        description: "start an interactive session: /spawn <domain> <goal>",
    },
    CommandSpec {
        insert: "/join ",
        name: "/join",
        description: "join a goal session by id (focus its input)",
    },
    CommandSpec {
        insert: "/back",
        name: "/back",
        description: "return focus to the primary chat",
    },
    CommandSpec {
        insert: "/session ",
        name: "/session …",
        description: "session subcommands (info, list, switch, close)",
    },
    CommandSpec {
        insert: "/session info",
        name: "/session info",
        description: "show active session",
    },
    CommandSpec {
        insert: "/session list",
        name: "/session list",
        description: "list conversations",
    },
    CommandSpec {
        insert: "/session switch ",
        name: "/session switch",
        description: "switch by conversation id prefix",
    },
    CommandSpec {
        insert: "/session close",
        name: "/session close",
        description: "close active session view",
    },
    CommandSpec {
        insert: "/quit",
        name: "/quit",
        description: "quit the client",
    },
    CommandSpec {
        insert: "/exit",
        name: "/exit",
        description: "quit the client (alias)",
    },
];

/// Progressive filter for slash input. Empty when input is not a slash command prefix.
///
/// Matching is case-insensitive prefix on `name` / `insert`. Typing more of a subcommand
/// narrows the list (e.g. `/th` → theme family, `/theme s` → set).
pub fn filter_commands(input: &str) -> Vec<&'static CommandSpec> {
    let query = slash_query(input);
    let Some(query) = query else {
        return Vec::new();
    };
    let q = query.to_ascii_lowercase();
    COMMAND_CATALOG
        .iter()
        .filter(|spec| {
            let name = spec.name.to_ascii_lowercase();
            let insert = spec.insert.to_ascii_lowercase();
            name.starts_with(&q) || insert.starts_with(&q)
        })
        .collect()
}

/// Whether the input should show a slash palette (starts with `/` on the first line).
pub fn is_slash_prefix(input: &str) -> bool {
    slash_query(input).is_some()
}

/// Tab-completion text for the current input + optional selected catalog index.
///
/// - selected match → that entry's `insert`
/// - single match → its `insert`
/// - multiple → longest common prefix of `insert` values (at least as long as current query)
pub fn complete_commands(input: &str, selected_index: usize) -> Option<String> {
    let matches = filter_commands(input);
    if matches.is_empty() {
        return None;
    }
    if let Some(spec) = matches.get(selected_index) {
        return Some(spec.insert.to_string());
    }
    if matches.len() == 1 {
        return Some(matches[0].insert.to_string());
    }
    let mut prefix = matches[0].insert.to_string();
    for m in matches.iter().skip(1) {
        prefix = common_prefix(&prefix, m.insert);
        if prefix.is_empty() {
            break;
        }
    }
    let query = slash_query(input).unwrap_or("");
    if prefix.len() > query.len() {
        Some(prefix)
    } else {
        // No additional chars; jump to selected (0) insert for a decisive Tab.
        Some(matches[0].insert.to_string())
    }
}

/// Dim "ghost" remainder of the selected match after the typed query.
///
/// Example: input `/hel`, selected `/help` → `Some("p")`. Empty when the typed
/// text already covers the insert (or there is no match). Used for inline
/// ghost-complete UX (Enter accepts without Tab).
pub fn ghost_suffix(input: &str, selected_index: usize) -> Option<String> {
    let matches = filter_commands(input);
    let spec = matches.get(selected_index)?;
    let query = slash_query(input)?;
    let insert = spec.insert;
    if !starts_with_ignore_ascii_case(insert, query) {
        return None;
    }
    // Slice by char count so multi-byte and case folds stay aligned on ASCII slash cmds.
    let rest: String = insert.chars().skip(query.chars().count()).collect();
    if rest.is_empty() { None } else { Some(rest) }
}

/// Full command text Enter should use when a palette match is selected:
/// the selected catalog `insert` (not the partially typed buffer).
pub fn accept_completion(input: &str, selected_index: usize) -> Option<String> {
    complete_commands(input, selected_index)
}

fn starts_with_ignore_ascii_case(haystack: &str, prefix: &str) -> bool {
    let mut h = haystack.chars();
    let mut p = prefix.chars();
    loop {
        match (h.next(), p.next()) {
            (_, None) => return true,
            (None, Some(_)) => return false,
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(&b) => {}
            _ => return false,
        }
    }
}

fn slash_query(input: &str) -> Option<&str> {
    let first = input.lines().next().unwrap_or(input).trim_start();
    if !first.starts_with('/') {
        return None;
    }
    // Trailing spaces are significant for subcommand discovery ("/theme " vs "/theme").
    // Only strip a single trailing newline artifact; keep spaces.
    Some(first.trim_end_matches('\r'))
}

fn common_prefix(a: &str, b: &str) -> String {
    let mut end = 0;
    for (ca, cb) in a.chars().zip(b.chars()) {
        if ca.eq_ignore_ascii_case(&cb) {
            end += ca.len_utf8();
        } else {
            break;
        }
    }
    // Prefer the casing from `a`.
    a[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_slash_lists_all() {
        assert_eq!(filter_commands("/").len(), COMMAND_CATALOG.len());
    }

    #[test]
    fn progressive_narrows_theme() {
        let m = filter_commands("/th");
        assert!(
            m.iter()
                .all(|s| s.name.starts_with("/th") || s.insert.starts_with("/th"))
        );
        assert!(m.iter().any(|s| s.name == "/theme"));
        let m2 = filter_commands("/theme s");
        assert!(m2.iter().any(|s| s.name == "/theme set"));
        assert!(!m2.iter().any(|s| s.name == "/theme list"));
    }

    #[test]
    fn non_slash_is_empty() {
        assert!(filter_commands("hello").is_empty());
        assert!(filter_commands("").is_empty());
    }

    #[test]
    fn complete_single_match() {
        assert_eq!(complete_commands("/hel", 0).as_deref(), Some("/help"));
    }

    #[test]
    fn complete_selected_index() {
        let matches = filter_commands("/session");
        assert!(matches.len() > 1);
        let idx = matches
            .iter()
            .position(|s| s.name == "/session list")
            .unwrap();
        assert_eq!(
            complete_commands("/session", idx).as_deref(),
            Some("/session list")
        );
    }

    #[test]
    fn ghost_suffix_shows_remainder() {
        assert_eq!(ghost_suffix("/hel", 0).as_deref(), Some("p"));
        assert_eq!(ghost_suffix("/help", 0), None);
    }

    #[test]
    fn ghost_suffix_follows_selection() {
        let matches = filter_commands("/session");
        let idx = matches
            .iter()
            .position(|s| s.name == "/session list")
            .unwrap();
        assert_eq!(ghost_suffix("/session", idx).as_deref(), Some(" list"));
    }

    #[test]
    fn accept_completion_is_selected_insert() {
        assert_eq!(accept_completion("/hel", 0).as_deref(), Some("/help"));
    }
}
