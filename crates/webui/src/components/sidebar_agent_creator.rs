//! Agents shelf + : find or create the singleton Agent Creator session.
//!
//! `POST /api/conversations` with `{"agent_creator": true}`. 201 the first time, 200 when the
//! flagged row already exists. No specialist picker.

use chat_client_contract::ConvHeader;
use dioxus::prelude::*;

/// Open or focus the Agent Creator. Accepts every 2xx so a reused session (200) is success.
pub(super) async fn open_agent_creator_session(api_base: &str) -> Result<ConvHeader, String> {
    let url = format!("{api_base}/api/conversations");
    let resp = reqwest::Client::new()
        .post(url)
        .json(&serde_json::json!({ "agent_creator": true }))
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
