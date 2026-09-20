//! **New Agent** create-path: pick an agent-eligible chat profile, POST create, land on Agents.

use chat_client_contract::ConvHeader;
use dioxus::prelude::*;
use serde::Deserialize;

use crate::components::picker::Picker;

/// One row of `GET /api/profiles` (subset used by New Agent).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ProfileRow {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub agent_eligible: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct ProfilesResponse {
    #[serde(default)]
    profiles: Vec<ProfileRow>,
}

fn label_for(row: &ProfileRow) -> String {
    match row.description.as_deref().filter(|d| !d.trim().is_empty()) {
        Some(desc) => format!("{}  —  {desc}", row.name),
        None => row.name.clone(),
    }
}

fn name_from_label(label: &str) -> &str {
    label.split("  —  ").next().unwrap_or(label).trim()
}

/// Chat profiles that Reading B will stamp as Agent (domain absent + `agent_eligible`).
pub(crate) fn agent_eligible_chat_profiles(rows: &[ProfileRow]) -> Vec<ProfileRow> {
    rows.iter()
        .filter(|p| p.domain.is_none() && p.agent_eligible)
        .cloned()
        .collect()
}

async fn fetch_agent_profiles(api_base: String) -> Result<Vec<ProfileRow>, String> {
    let url = format!("{api_base}/api/profiles");
    let body: ProfilesResponse = reqwest::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    Ok(agent_eligible_chat_profiles(&body.profiles))
}

/// `POST /api/conversations` with a profile so create_with_grant stamps Agent.
pub(crate) async fn create_agent_conversation(
    api_base: &str,
    profile: &str,
) -> Result<ConvHeader, String> {
    let url = format!("{api_base}/api/conversations");
    let resp = reqwest::Client::new()
        .post(url)
        .json(&serde_json::json!({ "profile": profile }))
        .send()
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!("Create refused (HTTP {status}): {detail}"));
    }
    resp.json::<ConvHeader>()
        .await
        .map_err(|e| format!("Bad create response: {e}"))
}

#[component]
pub fn NewAgentPicker(
    api_base: String,
    open: Signal<bool>,
    on_created: EventHandler<ConvHeader>,
) -> Element {
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut open = open;
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);

    let catalog = use_resource({
        let base = api_base.clone();
        move || fetch_agent_profiles(base.clone())
    });

    let (rows, load_error) = match &*catalog.read() {
        Some(Ok(rows)) => (rows.clone(), None),
        Some(Err(e)) => (Vec::new(), Some(e.clone())),
        None => (Vec::new(), None),
    };
    let loading = catalog.read().is_none();
    let items: Vec<String> = rows.iter().map(label_for).collect();

    let pick = {
        let base = api_base.clone();
        use_callback(move |label: String| {
            if busy() {
                return;
            }
            let profile = name_from_label(&label).to_string();
            busy.set(true);
            error.set(None);
            let base = base.clone();
            #[cfg(target_arch = "wasm32")]
            wasm_bindgen_futures::spawn_local(async move {
                match create_agent_conversation(&base, &profile).await {
                    Ok(header) => {
                        busy.set(false);
                        on_created.call(header);
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
                let _ = (base, profile);
                busy.set(false);
            }
        })
    };

    let status = if loading {
        Some("Loading agent profiles\u{2026}".to_string())
    } else if busy() {
        Some("Creating\u{2026}".to_string())
    } else if rows.is_empty() && load_error.is_none() {
        Some(
            "No agent chat profiles configured — add coding|life|researcher|operator under \
             [[session_profiles]] (no domain)."
                .to_string(),
        )
    } else {
        None
    };

    rsx! {
        Picker {
            title: "New Agent — pick a specialist profile",
            current: None::<String>,
            items,
            status,
            error: error().or(load_error),
            open,
            on_pick: move |label: String| pick.call(label),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, domain: Option<&str>, agent: bool) -> ProfileRow {
        ProfileRow {
            name: name.into(),
            description: None,
            domain: domain.map(str::to_owned),
            agent_eligible: agent,
        }
    }

    #[test]
    fn picker_keeps_only_agent_eligible_chat_hats() {
        let rows = vec![
            row("coding", None, true),
            row("chat-default", None, false),
            row("coding-unattended", Some("coding"), true), // pack — excluded
            row("life", None, true),
            row("operator", None, true),
        ];
        let got: Vec<_> = agent_eligible_chat_profiles(&rows)
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(got, vec!["coding", "life", "operator"]);
    }
}
