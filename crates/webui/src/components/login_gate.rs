//! Sign-in screen when credentials are configured and this browser has no session.

use dioxus::prelude::*;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct SessionResponse {
    pub authenticated: bool,
    pub login_required: bool,
    #[serde(default)]
    pub passkeys_enabled: bool,
    #[serde(default)]
    pub passkey_count: i64,
    #[serde(default)]
    pub can_register: bool,
    #[serde(default)]
    pub method: Option<String>,
}

#[component]
pub fn LoginGate(api_base: String, on_authenticated: EventHandler<()>) -> Element {
    let mut session_meta = use_signal(|| None::<SessionResponse>);
    let mut show_token = use_signal(|| false);
    let mut token = use_signal(String::new);
    let mut error = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);
    let mut enroll_passkey = use_signal(|| false);

    let base_probe = api_base.clone();
    use_effect(move || {
        let base_probe = base_probe.clone();
        spawn(async move {
            if let Ok(resp) = crate::http::session_status(&base_probe).await {
                if resp.status().is_success()
                    && let Ok(body) = resp.json::<SessionResponse>().await
                {
                    session_meta.set(Some(body.clone()));
                    if !body.passkeys_enabled {
                        show_token.set(true);
                    }
                }
            }
        });
    });

    let passkeys_enabled = session_meta().map(|s| s.passkeys_enabled).unwrap_or(false);
    let secure_ok = webauthn_available();

    let passkey_login = {
        let api_base = api_base.clone();
        move |_| {
            let api_base = api_base.clone();
            busy.set(true);
            error.set(None);
            spawn(async move {
                match passkey_sign_in(&api_base).await {
                    Ok(()) => {
                        busy.set(false);
                        on_authenticated.call(());
                    }
                    Err(msg) => {
                        busy.set(false);
                        error.set(Some(msg));
                    }
                }
            });
        }
    };

    let token_login = {
        let api_base = api_base.clone();
        move |_| {
            let api_base = api_base.clone();
            let value = token();
            if value.trim().is_empty() {
                error.set(Some("Enter the emergency token".into()));
                return;
            }
            busy.set(true);
            error.set(None);
            spawn(async move {
                match crate::http::login(&api_base, &value).await {
                    Ok(resp) if resp.status().is_success() => {
                        busy.set(false);
                        if passkeys_enabled && secure_ok {
                            enroll_passkey.set(true);
                        } else {
                            on_authenticated.call(());
                        }
                    }
                    Ok(resp) => {
                        busy.set(false);
                        let status = resp.status();
                        let message = resp
                            .text()
                            .await
                            .ok()
                            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                            .and_then(|v| {
                                v.get("error").and_then(|e| e.as_str()).map(str::to_owned)
                            })
                            .unwrap_or_else(|| format!("Sign-in failed ({status})"));
                        error.set(Some(message));
                    }
                    Err(err) => {
                        busy.set(false);
                        error.set(Some(err.to_string()));
                    }
                }
            });
        }
    };

    if enroll_passkey() {
        return rsx! {
            div { class: "login-gate-shell grok-page login-page",
                div { class: "login-gate-card grok-page-body card",
                    h1 { class: "grok-page-title", "Add a passkey for this device" }
                    p { class: "grok-page-subtitle",
                        "Use a passkey next time instead of the emergency token."
                    }
                    if let Some(msg) = error() {
                        p { class: "login-gate-error", "{msg}" }
                    }
                    div { class: "login-gate-actions",
                        button {
                            class: "lib-btn primary",
                            disabled: busy(),
                            onclick: move |_| {
                                let api_base = api_base.clone();
                                busy.set(true);
                                spawn(async move {
                                    match register_passkey(&api_base).await {
                                        Ok(()) => on_authenticated.call(()),
                                        Err(msg) => {
                                            busy.set(false);
                                            error.set(Some(msg));
                                        }
                                    }
                                });
                            },
                            "Add passkey"
                        }
                        button {
                            class: "lib-btn link",
                            disabled: busy(),
                            onclick: move |_| on_authenticated.call(()),
                            "Not now"
                        }
                    }
                }
            }
        };
    }

    rsx! {
        div { class: "login-gate-shell grok-page login-page",
            div { class: "login-gate-card grok-page-body card",
                h1 { class: "grok-page-title", "Sign in to Liberado" }
                if passkeys_enabled && secure_ok {
                    button {
                        class: "lib-btn primary login-passkey-btn",
                        disabled: busy(),
                        onclick: passkey_login,
                        "Sign in with passkey"
                    }
                } else if passkeys_enabled {
                    p { class: "grok-page-subtitle",
                        "Passkeys need https or localhost in this browser."
                    }
                }
                if !show_token() && passkeys_enabled && secure_ok {
                    button {
                        class: "lib-btn link login-token-toggle",
                        onclick: move |_| show_token.set(true),
                        "Use emergency token"
                    }
                }
                if show_token() || !passkeys_enabled || !secure_ok {
                    if !passkeys_enabled || !secure_ok {
                        p { class: "grok-page-subtitle login-token-hint",
                            if !secure_ok && passkeys_enabled {
                                "Passkeys need https — sign in with your emergency token."
                            } else {
                                "Sign in with your emergency token."
                            }
                        }
                    }
                    input {
                        class: "login-gate-input",
                        r#type: "password",
                        placeholder: "Emergency token",
                        value: "{token}",
                        disabled: busy(),
                        oninput: move |e| token.set(e.value()),
                    }
                    button {
                        class: "lib-btn primary",
                        disabled: busy(),
                        onclick: token_login,
                        if busy() { "Signing in…" } else { "Sign in" }
                    }
                }
                if let Some(msg) = error() {
                    p { class: "login-gate-error", "{msg}" }
                }
            }
        }
    }
}

pub async fn probe_session(api_base: &str) -> Result<SessionResponse, String> {
    let resp = crate::http::session_status(api_base)
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("session probe failed: {}", resp.status()));
    }
    resp.json().await.map_err(|e| e.to_string())
}

fn webauthn_available() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window().is_some_and(|w| w.is_secure_context())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        false
    }
}

async fn passkey_sign_in(api_base: &str) -> Result<(), String> {
    let start = crate::http::post_empty(&format!("{api_base}/api/auth/passkey/login/start"))
        .await
        .map_err(|e| e.to_string())?;
    if !start.status().is_success() {
        return Err(read_error(start).await);
    }
    let body: serde_json::Value = start.json().await.map_err(|e| e.to_string())?;
    let options = body
        .get("options")
        .ok_or_else(|| "missing passkey options".to_string())?;
    let cred = crate::webauthn::get_passkey(&options.to_string()).await?;
    let cred_val: serde_json::Value =
        serde_json::from_str(&cred).map_err(|_| "invalid passkey payload".to_string())?;
    let finish = crate::http::post_json(
        &format!("{api_base}/api/auth/passkey/login/finish"),
        &serde_json::json!({ "credential": cred_val }),
    )
    .await
    .map_err(|e| e.to_string())?;
    if finish.status().is_success() {
        Ok(())
    } else {
        Err(read_error(finish).await)
    }
}

pub(crate) async fn register_passkey(api_base: &str) -> Result<(), String> {
    let start = crate::http::post_empty(&format!("{api_base}/api/auth/passkey/register/start"))
        .await
        .map_err(|e| e.to_string())?;
    if !start.status().is_success() {
        return Err(read_error(start).await);
    }
    let body: serde_json::Value = start.json().await.map_err(|e| e.to_string())?;
    let options = body
        .get("options")
        .ok_or_else(|| "missing passkey options".to_string())?;
    let cred = crate::webauthn::create_passkey(&options.to_string()).await?;
    let cred_val: serde_json::Value =
        serde_json::from_str(&cred).map_err(|_| "invalid passkey payload".to_string())?;
    let finish = crate::http::post_json(
        &format!("{api_base}/api/auth/passkey/register/finish"),
        &serde_json::json!({ "credential": cred_val }),
    )
    .await
    .map_err(|e| e.to_string())?;
    if finish.status().is_success() {
        Ok(())
    } else {
        Err(read_error(finish).await)
    }
}

async fn read_error(resp: reqwest::Response) -> String {
    let status = resp.status();
    resp.text()
        .await
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
        .unwrap_or_else(|| format!("Request failed ({status})"))
}
