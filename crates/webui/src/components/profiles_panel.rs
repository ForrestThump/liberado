//! The Status screen's read-only list of configured session profiles.
//!
//! **Read-only on purpose.** Everything else on the Status screen is daemon-scoped — uptime, the
//! vault, the watcher, the model — and a session profile is per *conversation*. A switcher here
//! would have to ask which conversation you meant, which is the chat's question, not this screen's.
//! The same distinction that keeps `/model` (process-wide hot-swap) and `/profile` (this chat)
//! apart.
//!
//! What does belong here is the config itself: which profiles exist and what each one allows. That
//! is a property of the daemon, and it is the question you actually have on this screen — "what
//! could I put a chat into", not "put this chat into one".

use dioxus::prelude::*;

use crate::components::profile_browser::ProfileRow;

async fn fetch_profiles(api_base: String) -> Result<Vec<ProfileRow>, String> {
    let url = format!("{api_base}/api/profiles");
    let body: serde_json::Value = reqwest::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    serde_json::from_value(body.get("profiles").cloned().unwrap_or_default())
        .map_err(|e| format!("Bad response: {e}"))
}

/// How a profile's routing reads in one phrase.
///
/// A pack profile and a chat profile are different animals — one runs `/spawn` work, the other runs
/// a conversation — and the list is much harder to read if that is left implicit.
fn routing_of(row: &ProfileRow) -> String {
    match (&row.domain, row.delegation) {
        (Some(domain), _) => format!("pack: {domain}"),
        (None, Some(false)) => "chat · no dispatch".to_string(),
        (None, _) => "chat".to_string(),
    }
}

#[component]
pub fn ProfilesPanel(api_base: String) -> Element {
    let profiles = use_resource(move || fetch_profiles(api_base.clone()));

    rsx! {
        div {
            class: "card",
            div {
                class: "card-header",
                span { "SESSION PROFILES" }
            }
            div {
                class: "card-body",
                match &*profiles.read() {
                    Some(Ok(rows)) if rows.is_empty() => rsx! {
                        p { class: "empty-panel",
                            "None configured. Add a [[session_profiles]] entry in topology.toml."
                        }
                    },
                    Some(Ok(rows)) => rsx! {
                        for row in rows.iter() {
                            div {
                                key: "{row.name}",
                                class: "profile-row",
                                div {
                                    class: "profile-row-top",
                                    span { class: "profile-row-name", "{row.name}" }
                                    span { class: "profile-row-routing", "{routing_of(row)}" }
                                }
                                if let Some(desc) = row.description.as_deref() {
                                    p { class: "profile-row-desc", "{desc}" }
                                }
                            }
                        }
                        // Says where the control *is*, rather than leaving someone hunting this
                        // screen for a button that is deliberately not on it.
                        p { class: "profile-row-note",
                            "Read-only. Switch a conversation's profile from the chat, or with /profile."
                        }
                    },
                    Some(Err(e)) => rsx! {
                        p { class: "empty-panel", "Error: {e}" }
                    },
                    None => rsx! {
                        p { class: "empty-panel", "Loading\u{2026}" }
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(domain: Option<&str>, delegation: Option<bool>) -> ProfileRow {
        ProfileRow {
            name: "x".into(),
            description: None,
            domain: domain.map(str::to_string),
            delegation,
        }
    }

    /// A pack profile and a chat profile must not read the same, or the list invites putting a
    /// conversation onto a grant written for an unattended run.
    #[test]
    fn routing_distinguishes_pack_profiles_from_chat_profiles() {
        assert_eq!(routing_of(&row(Some("coding"), None)), "pack: coding");
        assert_eq!(routing_of(&row(None, Some(false))), "chat · no dispatch");
        assert_eq!(routing_of(&row(None, Some(true))), "chat");
        // Unset delegation means "inherit the daemon default", which is not the same claim as
        // "no dispatch" — saying so would be a lie whenever the daemon has delegation on.
        assert_eq!(routing_of(&row(None, None)), "chat");
    }
}
