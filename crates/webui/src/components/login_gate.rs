//! Sign-in screen when `[api]` credentials are configured and this browser has no session.

use dioxus::prelude::*;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct SessionResponse {
    pub authenticated: bool,
    pub login_required: bool,
}

#[component]
pub fn LoginGate(
    api_base: String,
    on_authenticated: EventHandler<()>,
) -> Element {
    let mut token = use_signal(String::new);
    let mut error = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);

    let submit = move |_| {
        let api_base = api_base.clone();
        let value = token();
        if value.trim().is_empty() {
            error.set(Some("paste the human token".into()));
            return;
        }
        busy.set(true);
        error.set(None);
        spawn(async move {
            match crate::http::login(&api_base, &value).await {
                Ok(resp) if resp.status().is_success() => {
                    busy.set(false);
                    on_authenticated.call(());
                }
                Ok(resp) => {
                    busy.set(false);
                    let status = resp.status();
                    let message = resp
                        .text()
                        .await
                        .ok()
                        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
                        .unwrap_or_else(|| format!("sign-in failed ({status})"));
                    error.set(Some(message));
                }
                Err(err) => {
                    busy.set(false);
                    error.set(Some(err.to_string()));
                }
            }
        });
    };

    rsx! {
        div { class: "login-gate",
            h1 { "Sign in" }
            p { class: "login-gate-hint",
                "Paste the human token from topology (`[api] human`). It is stored only in an HttpOnly cookie."
            }
            if let Some(msg) = error() {
                p { class: "login-gate-error", "{msg}" }
            }
            textarea {
                class: "login-gate-input",
                placeholder: "human token",
                value: "{token}",
                disabled: busy(),
                oninput: move |e| token.set(e.value()),
            }
            button {
                class: "login-gate-submit",
                disabled: busy(),
                onclick: submit,
                if busy() { "Signing in…" } else { "Sign in" }
            }
        }
    }
}

/// Load session state once; call `on_need_login` when the UI must block behind [`LoginGate`].
pub async fn probe_session(api_base: &str) -> Result<SessionResponse, String> {
    let resp = crate::http::session_status(api_base)
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("session probe failed: {}", resp.status()));
    }
    resp.json()
        .await
        .map_err(|e| e.to_string())
}
