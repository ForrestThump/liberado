use dioxus::prelude::*;

use crate::types::VaultInfo;

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
            class: "rounded-xl border border-gray-800 bg-gray-900/50",
            div {
                class: "px-5 py-4 border-b border-gray-800",
                h2 { class: "text-sm font-semibold uppercase tracking-wider text-gray-400",
                    "Vault"
                }
            }
            div {
                class: "p-5",
                match &*info.read() {
                    Some(Ok(v)) => rsx! {
                        div { class: "space-y-4",
                            VaultRow { label: "Root", value: &v.root }
                            VaultRow { label: "Notes", value: &v.note_count.to_string() }
                            VaultRow { label: "Watcher", value: if v.watcher_active { "Active" } else { "Inactive" } }
                        }
                    },
                    Some(Err(e)) => rsx! {
                        p { class: "text-red-500 text-sm", "Error: {e}" }
                    },
                    None => rsx! {
                        p { class: "text-gray-600 text-sm", "Loading..." }
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
            class: "flex items-center justify-between",
            span { class: "text-sm text-gray-500", "{label}" }
            span { class: "text-sm font-mono text-gray-300 truncate ml-4", "{value}" }
        }
    }
}
