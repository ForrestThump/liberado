use dioxus::prelude::*;

use chat_client_contract::VaultInfo;

async fn fetch_vault(api_base: String) -> Result<VaultInfo, String> {
    let url = format!("{api_base}/api/vault");
    let resp = reqwest::get(&url).await.map_err(|e| format!("{e}"))?;
    let info: VaultInfo = resp.json().await.map_err(|e| format!("{e}"))?;
    Ok(info)
}

#[component]
pub fn VaultPanel(api_base: String) -> Element {
    let info = use_resource(move || fetch_vault(api_base.clone()));

    rsx! {
        div {
            class: "card",
            div {
                class: "card-header",
                h2 { "Vault" }
            }
            div {
                class: "card-body",
                match &*info.read() {
                    Some(Ok(v)) => rsx! {
                        div {
                            VaultRow { label: "Root", value: &v.root }
                            VaultRow { label: "Notes", value: &v.note_count.to_string() }
                            VaultRow { label: "Watcher", value: if v.watcher_active { "Active" } else { "Inactive" } }
                        }
                    },
                    Some(Err(e)) => rsx! {
                        p { class: "empty-panel", "Error: {e}" }
                    },
                    None => rsx! {
                        p { class: "empty-panel", "Loading..." }
                    },
                }
            }
        }
    }
}

#[component]
fn VaultRow(label: String, value: String) -> Element {
    rsx! {
        div {
            class: "vault-row",
            span { class: "vault-label", "{label}" }
            span { class: "vault-value", "{value}" }
        }
    }
}
