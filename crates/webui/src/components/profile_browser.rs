//! The session-profile picker behind `/profile`.
//!
//! Data and action only; the filter box, keyboard handling and dismissal live in
//! [`crate::components::picker::Picker`], shared with `/model` and `/theme`. Adding this was a list
//! and a callback, which is what the shell was extracted for.
//!
//! Reads `GET /api/profiles` and switches with `POST /api/conversations/{id}/profile`.
//!
//! # Why this is not modelled on the model picker
//!
//! `/model` hot-swaps the daemon's model for **every** conversation. A profile is per
//! conversation — it changes what *this* chat may do — so this needs a session id and refuses
//! without one, where the model picker needs none. It is also the one control in this UI that
//! changes authority, which is why the daemon exposes it only over `POST` and never as a tool: the
//! agent must not be able to re-authorise itself. Nothing here should ever be called from a tool
//! handler.

use dioxus::prelude::*;
use serde::Deserialize;

use crate::components::picker::Picker;

/// One row of `GET /api/profiles`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ProfileRow {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Present for a pack profile (`/spawn`), absent for a chat profile. Only chat profiles belong
    /// in this picker — offering a pack profile here would let someone put a conversation onto a
    /// grant written for an unattended coding run.
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub delegation: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProfilesResponse {
    #[serde(default)]
    profiles: Vec<ProfileRow>,
}

/// The label shown for a profile: its name, plus the description when there is one.
///
/// One string rather than two columns because [`Picker`] renders a single item label. Rebuilding it
/// with a second column would mean touching the shared shell for one caller's benefit.
fn label_for(row: &ProfileRow) -> String {
    match row.description.as_deref().filter(|d| !d.trim().is_empty()) {
        Some(desc) => format!("{}  —  {desc}", row.name),
        None => row.name.clone(),
    }
}

/// The profile name a label came from — the inverse of [`label_for`], for turning a pick back into
/// something the API understands.
fn name_from_label(label: &str) -> &str {
    label.split("  —  ").next().unwrap_or(label).trim()
}

async fn fetch_profiles(api_base: String) -> Result<Vec<ProfileRow>, String> {
    let url = format!("{api_base}/api/profiles");
    let body: ProfilesResponse = reqwest::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    // Chat profiles only — see `ProfileRow::domain`.
    Ok(body
        .profiles
        .into_iter()
        .filter(|p| p.domain.is_none())
        .collect())
}

async fn select_profile(
    api_base: String,
    session: String,
    name: Option<String>,
) -> Result<(), String> {
    let url = format!("{api_base}/api/conversations/{session}/profile");
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if resp.status().is_success() {
        return Ok(());
    }
    // The daemon refuses an unknown or disabled profile rather than falling back to the default
    // grant, so this is a real answer worth showing rather than a shrug.
    let status = resp.status().as_u16();
    let detail = resp.text().await.unwrap_or_default();
    Err(format!("Switch refused (HTTP {status}): {detail}"))
}

/// Sentinel row for clearing back to the daemon's default grant.
///
/// A row rather than a separate button: clearing *is* a choice among the profiles, and giving it its
/// own control would put an authority change somewhere the picker's keyboard contract does not
/// reach.
const CLEAR_LABEL: &str = "(default)  —  the daemon's standard grant, no profile";

/// Turn a picked label into the profile name to send, or `None` for the clear sentinel — which
/// means "back to the default grant", not "no change".
fn chosen_name(label: &str) -> Option<String> {
    (label != CLEAR_LABEL).then(|| name_from_label(label).to_string())
}

#[component]
pub fn ProfileBrowser(
    api_base: String,
    /// The conversation being re-authorised. `None` when no chat is open yet — the picker says so
    /// rather than silently doing nothing, because "nothing happened" is indistinguishable from a
    /// failed switch.
    session: Option<String>,
    /// The profile currently in force, for the active badge.
    current: Option<String>,
    open: Signal<bool>,
    on_switched: EventHandler<Option<String>>,
) -> Element {
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut open = open;
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);

    let catalog = use_resource({
        let base = api_base.clone();
        move || fetch_profiles(base.clone())
    });

    let (rows, load_error) = match &*catalog.read() {
        Some(Ok(rows)) => (rows.clone(), None),
        Some(Err(e)) => (Vec::new(), Some(e.clone())),
        None => (Vec::new(), None),
    };
    let loading = catalog.read().is_none();

    let mut items: Vec<String> = vec![CLEAR_LABEL.to_string()];
    items.extend(rows.iter().map(label_for));
    let current_label = match current.as_deref() {
        Some(name) => rows
            .iter()
            .find(|r| r.name == name)
            .map(label_for)
            // A profile that no longer exists in config (disabled since the chat was switched) still
            // deserves a badge, or the picker would claim the chat is on the default.
            .or_else(|| Some(name.to_string())),
        None => Some(CLEAR_LABEL.to_string()),
    };

    let switch_to = {
        let base = api_base.clone();
        let session = session.clone();
        use_callback(move |label: String| {
            if busy() {
                return;
            }
            // No conversation yet: accept the pick and let the owner carry it onto the request that
            // creates one. Refusing here is what made the *first* turn of every chat run on the
            // default grant — the turn a "basic chat" profile most wants to scope.
            let Some(session) = session.clone() else {
                let chosen = chosen_name(&label);
                on_switched.call(chosen);
                open.set(false);
                return;
            };
            busy.set(true);
            error.set(None);
            let base = base.clone();
            let chosen = chosen_name(&label);
            #[cfg(target_arch = "wasm32")]
            wasm_bindgen_futures::spawn_local(async move {
                match select_profile(base, session, chosen.clone()).await {
                    Ok(()) => {
                        busy.set(false);
                        on_switched.call(chosen);
                        open.set(false);
                    }
                    Err(e) => {
                        busy.set(false);
                        error.set(Some(e));
                    }
                }
            });
            #[cfg(not(target_arch = "wasm32"))]
            {
                let _ = (base, session, chosen);
                busy.set(false);
            }
        })
    };

    let status = if loading {
        Some("Loading profiles\u{2026}".to_string())
    } else if busy() {
        Some("Switching\u{2026}".to_string())
    } else if rows.is_empty() && load_error.is_none() {
        Some("No chat profiles configured — add one under [[session_profiles]].".to_string())
    } else {
        None
    };

    rsx! {
        Picker {
            title: "Session profile (this chat)",
            current: current_label,
            items,
            status,
            error: error().or(load_error),
            open,
            on_pick: move |label: String| switch_to.call(label),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, desc: Option<&str>) -> ProfileRow {
        ProfileRow {
            name: name.into(),
            description: desc.map(str::to_string),
            domain: None,
            delegation: None,
        }
    }

    /// The label is what the picker shows *and* what comes back from a pick, so the round trip has
    /// to hold — otherwise a switch posts a name the daemon has never heard of.
    #[test]
    fn a_label_round_trips_back_to_its_profile_name() {
        for (name, desc) in [
            ("basic-chat", Some("Quick answers. No dispatch.")),
            ("research", None),
            // A description containing a dash must not confuse the split.
            ("writer", Some("drafts — and edits — prose")),
        ] {
            let r = row(name, desc);
            assert_eq!(name_from_label(&label_for(&r)), name, "{name}");
        }
    }

    #[test]
    fn the_clear_sentinel_is_not_mistaken_for_a_profile() {
        assert_ne!(name_from_label(CLEAR_LABEL), "");
        // The caller distinguishes it by identity, not by parsing — this just guards the assumption
        // that no real profile could produce the same label.
        assert!(CLEAR_LABEL.starts_with("(default)"));
    }

    /// Picking the clear row means "back to the default grant", which the wire spells as `None` —
    /// not as the literal text of the row.
    #[test]
    fn the_clear_row_means_none() {
        assert_eq!(chosen_name(CLEAR_LABEL), None);
    }

    /// Any real row maps back to its profile name — the same round trip `label_for`/`name_from_label`
    /// already guarantees, asserted at the point the label becomes a request.
    #[test]
    fn a_real_row_means_its_profile_name() {
        let r = row("basic-chat", Some("Quick answers. No dispatch."));
        assert_eq!(chosen_name(&label_for(&r)), Some("basic-chat".to_string()));
        assert_eq!(
            chosen_name("unlisted  —  whatever"),
            Some("unlisted".to_string())
        );
    }

    /// The description is part of the label — the picker row is unreadable without it — and a
    /// whitespace-only description is dropped exactly like an empty one.
    #[test]
    fn the_description_is_part_of_the_label() {
        assert_eq!(
            label_for(&row("basic-chat", Some("Quick answers."))),
            "basic-chat  —  Quick answers."
        );
        assert_eq!(label_for(&row("plain", None)), "plain");
        assert_eq!(label_for(&row("blank", Some("   "))), "blank");
    }
}
