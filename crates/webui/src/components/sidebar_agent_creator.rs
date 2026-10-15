//! Agents shelf + : open a new Agent Creator session on every click.
//!
//! The live daemon treats `{"agent_creator": true}` as find-or-create of one row, so a second
//! click would reopen that session. This request omits that flag. `profile: operator` is the
//! creator grant, and the daemon's create path mints a new conversation each time.

use chat_client_contract::ConvHeader;
use dioxus::prelude::*;

/// Profile the creator session runs under. Matches the daemon's creator grant (`operator`),
/// which is what puts `create_agent` on the face and stamps the Agents shelf.
pub(super) const CREATOR_PROFILE: &str = "operator";

/// Sidebar title. `stamp` is unique per click so two creator rows do not share a name.
pub(super) fn fresh_creator_title(stamp: &str) -> String {
    let stamp = stamp.trim();
    if stamp.is_empty() {
        "Agent Creator".to_string()
    } else {
        format!("Agent Creator {stamp}")
    }
}

/// Body of `POST /api/conversations` for one fresh creator.
///
/// No `agent_creator` field: on the live daemon that field selects the singleton.
pub(super) fn fresh_creator_body(stamp: &str) -> serde_json::Value {
    serde_json::json!({
        "profile": CREATOR_PROFILE,
        "title": fresh_creator_title(stamp),
    })
}

/// Open a new Agent Creator. A 2xx is a new conversation header, not a reused row.
pub(super) async fn open_agent_creator_session(
    api_base: &str,
    stamp: &str,
) -> Result<ConvHeader, String> {
    let url = format!("{api_base}/api/conversations");
    let resp = reqwest::Client::new()
        .post(url)
        .json(&fresh_creator_body(stamp))
        .send()
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!(
            "Could not open Agent Creator (HTTP {status}): {detail}"
        ));
    }
    resp.json::<ConvHeader>()
        .await
        .map_err(|e| format!("Bad create response: {e}"))
}

/// Create-path failure for the Agents + control. Row action errors stay on the row.
#[component]
pub(super) fn CreatorError(message: Option<String>) -> Element {
    if let Some(err) = message {
        rsx! {
            p { class: "sidebar-empty", "{err}" }
        }
    } else {
        rsx! {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The live daemon's `agent_creator: true` body find-or-creates one row. A second click
    /// must not send that flag, or the previous creator session comes back.
    #[test]
    fn each_click_requests_a_new_creator_not_the_singleton() {
        let first = fresh_creator_body("1");
        let second = fresh_creator_body("2");
        assert!(first.get("agent_creator").is_none(), "{first}");
        assert!(second.get("agent_creator").is_none(), "{second}");
        assert_eq!(first["profile"], CREATOR_PROFILE);
        assert_eq!(second["profile"], CREATOR_PROFILE);
        assert_ne!(first["title"], second["title"]);
        assert_eq!(first["title"], "Agent Creator 1");
        assert_eq!(second["title"], "Agent Creator 2");
    }

    #[test]
    fn a_blank_stamp_still_names_the_creator() {
        assert_eq!(fresh_creator_title("   "), "Agent Creator");
        let body = fresh_creator_body("");
        assert_eq!(body["title"], "Agent Creator");
        assert!(body.get("agent_creator").is_none());
    }
}
