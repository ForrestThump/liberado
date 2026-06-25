use dioxus::prelude::*;

use crate::components::reactions::ReactionsPanel;
use crate::components::vault::VaultPanel;
use crate::types::DaemonStatus;

async fn fetch_status(api_base: String) -> Result<DaemonStatus, String> {
    let url = format!("{api_base}/api/status");
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    let status: DaemonStatus = resp
        .json()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    Ok(status)
}

#[component]
pub fn Dashboard(api_base: String) -> Element {
    let status = use_resource({
        let base = api_base.clone();
        move || fetch_status(base.clone())
    });

    rsx! {
        div {
            class: "space-y-8",

            match &*status.read() {
                Some(Ok(s)) => rsx! {
                    StatusBanner { status: s.clone() }
                    div {
                        class: "grid grid-cols-1 lg:grid-cols-2 gap-6",
                        VaultPanel { api_base: api_base.clone() }
                        ReactionsPanel { api_base: api_base.clone() }
                    }
                },
                Some(Err(e)) => rsx! {
                    div {
                        class: "rounded-xl border border-red-800 bg-red-950/50 p-6 text-center",
                        p { class: "text-red-400 font-medium", "Connection Error" }
                        p { class: "text-sm text-red-600 mt-1", "{e}" }
                        p { class: "text-xs text-gray-600 mt-4",
                            "Ensure the daemon server is running (liberado serve <vault>)"
                        }
                    }
                },
                None => rsx! {
                    div {
                        class: "flex items-center justify-center py-20",
                        div {
                            class: "animate-spin rounded-full h-10 w-10 border-2 border-indigo-500 border-t-transparent"
                        }
                    }
                },
            }
        }
    }
}

#[component]
fn StatusBanner(status: DaemonStatus) -> Element {
    let color = if status.running {
        "bg-emerald-950/50 border-emerald-800"
    } else {
        "bg-red-950/50 border-red-800"
    };
    let dot = if status.running {
        "bg-emerald-400"
    } else {
        "bg-red-400"
    };
    let text = if status.running { "Running" } else { "Stopped" };

    rsx! {
        div {
            class: "rounded-xl border {color} p-6",
            div {
                class: "flex items-center justify-between",
                div {
                    class: "flex items-center gap-3",
                    div { class: "w-3 h-3 rounded-full {dot}" }
                    span { class: "text-lg font-semibold", "{text}" }
                }
                span { class: "text-sm text-gray-500",
                    "uptime: {format_uptime(status.uptime_seconds)}"
                }
            }
            div {
                class: "mt-4 grid grid-cols-2 sm:grid-cols-4 gap-4 text-sm",
                Stat { label: "Vault", value: &status.vault_path }
                Stat { label: "Watcher", value: bool_label(status.watcher_active) }
                Stat { label: "Dispatcher", value: bool_label(status.dispatcher_attached) }
                Stat { label: "Reactions", value: &status.reactions_seen.to_string() }
            }
        }
    }
}

#[component]
fn Stat(label: String, value: String) -> Element {
    rsx! {
        div {
            class: "bg-gray-900 rounded-lg px-3 py-2",
            p { class: "text-xs text-gray-500 uppercase tracking-wider", "{label}" }
            p { class: "text-sm font-medium text-gray-200 truncate", "{value}" }
        }
    }
}

fn bool_label(v: bool) -> &'static str {
    if v { "✓ Enabled" } else { "✗ Disabled" }
}

fn format_uptime(secs: Option<u64>) -> String {
    match secs {
        Some(s) => {
            let h = s / 3600;
            let m = (s % 3600) / 60;
            let sec = s % 60;
            format!("{h}h {m}m {sec}s")
        }
        None => "—".into(),
    }
}
