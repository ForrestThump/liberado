use dioxus::prelude::*;

use crate::components::reactions::ReactionsPanel;
use crate::components::vault::VaultPanel;
use chat_client_contract::DaemonStatus;

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
            class: "dashboard",

            match &*status.read() {
                Some(Ok(s)) => rsx! {
                    StatusBanner { status: s.clone() }
                    div {
                        class: "dashboard-grid",
                        VaultPanel { api_base: api_base.clone() }
                        ReactionsPanel { api_base: api_base.clone() }
                    }
                },
                Some(Err(e)) => rsx! {
                    div {
                        class: "error-card",
                        p { class: "error-title", "Connection Error" }
                        p { class: "error-detail", "{e}" }
                        p { class: "error-hint",
                            "Ensure the daemon server is running (liberado serve <vault>)"
                        }
                    }
                },
                None => rsx! {
                    div {
                        class: "loading-center",
                        div { class: "spinner" }
                    }
                },
            }
        }
    }
}

#[component]
fn StatusBanner(status: DaemonStatus) -> Element {
    let banner_cls = if status.running {
        "status-banner online"
    } else {
        "status-banner offline"
    };
    let dot_cls = if status.running {
        "status-dot online"
    } else {
        "status-dot offline"
    };
    let text = if status.running { "Running" } else { "Stopped" };

    rsx! {
        div {
            class: "{banner_cls}",
            div {
                class: "status-banner-top",
                div {
                    class: "status-banner-left",
                    div { class: "{dot_cls}" }
                    span { class: "status-label", "{text}" }
                }
                span { class: "status-uptime",
                    "uptime: {format_uptime(status.uptime_seconds)}"
                }
            }
            div {
                class: "status-stats",
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
            class: "stat-tile",
            p { class: "stat-label", "{label}" }
            p { class: "stat-value", "{value}" }
        }
    }
}

fn bool_label(v: bool) -> &'static str {
    if v { "✓ Enabled" } else { "✗ Disabled" }
}

fn format_uptime(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let sec = secs % 60;
    format!("{h}h {m}m {sec}s")
}
