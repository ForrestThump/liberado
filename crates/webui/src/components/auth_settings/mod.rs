//! Passkeys and signed-in sessions on the Notifications settings page.

mod api;

use dioxus::prelude::*;

use self::api::{
    AuthSessionRow, PasskeyRow, delete_passkey, fetch_passkeys, fetch_sessions,
    format_session_method, revoke_session, session_method,
};
use crate::components::notifications::model::{StatusLine, format_added_date};

#[component]
fn PasskeyRemoveButton(
    api_base: String,
    id: i64,
    on_message: EventHandler<StatusLine>,
    on_refresh: EventHandler<()>,
) -> Element {
    rsx! {
        button {
            class: "lib-btn danger",
            r#type: "button",
            onclick: move |_| {
                let base = api_base.clone();
                spawn(async move {
                    if let Err(err) = delete_passkey(&base, id).await {
                        on_message.call(StatusLine::Error(err));
                    } else {
                        on_message.call(StatusLine::Success("Passkey removed.".into()));
                        on_refresh.call(());
                    }
                });
            },
            "Remove"
        }
    }
}

#[component]
fn SessionRevokeButton(
    api_base: String,
    public_id: String,
    on_message: EventHandler<StatusLine>,
    on_refresh: EventHandler<()>,
) -> Element {
    rsx! {
        button {
            class: "lib-btn link",
            r#type: "button",
            onclick: move |_| {
                let base = api_base.clone();
                let id = public_id.clone();
                spawn(async move {
                    if let Err(err) = revoke_session(&base, &id).await {
                        on_message.call(StatusLine::Error(err));
                    } else {
                        on_message.call(StatusLine::Success("Session signed out.".into()));
                        on_refresh.call(());
                    }
                });
            },
            "Sign out"
        }
    }
}

#[component]
pub fn AuthSettingsSection(api_base: String, mut line: Signal<Option<StatusLine>>) -> Element {
    let mut passkeys = use_signal(Vec::<PasskeyRow>::new);
    let mut sessions = use_signal(Vec::<AuthSessionRow>::new);
    let mut show_token_for_register = use_signal(|| false);
    let mut register_token = use_signal(String::new);
    let mut register_busy = use_signal(|| false);
    let mut refresh_epoch = use_signal(|| 0u64);
    let base_for_effect = api_base.clone();
    let token_register_base = api_base.clone();
    let add_passkey_base = api_base.clone();
    let list_base = api_base.clone();

    use_effect(move || {
        let _ = refresh_epoch();
        let base = base_for_effect.clone();
        spawn(async move {
            if let Ok(rows) = fetch_passkeys(&base).await {
                passkeys.set(rows);
            }
            if let Ok(rows) = fetch_sessions(&base).await {
                sessions.set(rows);
            }
        });
    });

    let on_message = EventHandler::new(move |msg: StatusLine| line.set(Some(msg)));
    let on_refresh = EventHandler::new(move |_| refresh_epoch.set(refresh_epoch() + 1));

    rsx! {
        h2 { class: "notifications-section-title", "Passkeys" }
        p { class: "notifications-hint", "Sign in with a passkey instead of the emergency token." }
        if show_token_for_register() {
            p { class: "notifications-hint",
                "Adding a passkey needs the emergency token when you are signed in with a passkey."
            }
            input {
                class: "login-gate-input",
                r#type: "password",
                placeholder: "Emergency token",
                value: "{register_token}",
                disabled: register_busy(),
                oninput: move |e| register_token.set(e.value()),
            }
            div { class: "notifications-device-actions",
                button {
                    class: "lib-btn primary",
                    r#type: "button",
                    disabled: register_busy(),
                    onclick: move |_| {
                        let api_base = token_register_base.clone();
                        let token = register_token();
                        spawn(async move {
                            if token.trim().is_empty() {
                                on_message.call(StatusLine::Error(
                                    "Enter the emergency token.".into(),
                                ));
                                return;
                            }
                            register_busy.set(true);
                            match crate::http::login(&api_base, &token).await {
                                Ok(resp) if resp.status().is_success() => {
                                    match crate::components::login_gate::register_passkey(&api_base)
                                        .await
                                    {
                                        Ok(()) => {
                                            show_token_for_register.set(false);
                                            register_token.set(String::new());
                                            register_busy.set(false);
                                            on_message.call(StatusLine::Success(
                                                "Passkey added.".into(),
                                            ));
                                            on_refresh.call(());
                                        }
                                        Err(err) => {
                                            register_busy.set(false);
                                            on_message.call(StatusLine::Error(err));
                                        }
                                    }
                                }
                                Ok(resp) => {
                                    register_busy.set(false);
                                    on_message.call(StatusLine::Error(format!(
                                        "Sign-in failed ({})",
                                        resp.status()
                                    )));
                                }
                                Err(err) => {
                                    register_busy.set(false);
                                    on_message.call(StatusLine::Error(err.to_string()));
                                }
                            }
                        });
                    },
                    if register_busy() { "Adding…" } else { "Continue" }
                }
                button {
                    class: "lib-btn link",
                    r#type: "button",
                    onclick: move |_| show_token_for_register.set(false),
                    "Cancel"
                }
            }
        }
        if !show_token_for_register() {
            button {
                class: "lib-btn primary",
                r#type: "button",
                disabled: register_busy(),
                onclick: move |_| {
                    let api_base = add_passkey_base.clone();
                    spawn(async move {
                        match session_method(&api_base).await {
                            Ok(Some(m)) if m == "token" => {
                                register_busy.set(true);
                                match crate::components::login_gate::register_passkey(&api_base).await
                                {
                                    Ok(()) => {
                                        register_busy.set(false);
                                        on_message.call(StatusLine::Success(
                                            "Passkey added.".into(),
                                        ));
                                        on_refresh.call(());
                                    }
                                    Err(err) => {
                                        register_busy.set(false);
                                        on_message.call(StatusLine::Error(err));
                                    }
                                }
                            }
                            Ok(_) => show_token_for_register.set(true),
                            Err(err) => on_message.call(StatusLine::Error(err)),
                        }
                    });
                },
                "Add passkey"
            }
        }
        if !passkeys().is_empty() {
            ul { class: "plugin-list",
                for row in passkeys().iter().cloned() {
                    li {
                        class: "plugin-row",
                        key: "pk-{row.id}",
                        div {
                            class: "plugin-row-main",
                            span { class: "plugin-name", "{row.name}" }
                            div { class: "notifications-row-actions",
                                PasskeyRemoveButton {
                                    api_base: list_base.clone(),
                                    id: row.id,
                                    on_message,
                                    on_refresh,
                                }
                            }
                        }
                        p { class: "plugin-meta",
                            "Added {format_added_date(&row.created_at)}"
                            if let Some(last) = row.last_used_at.as_ref() {
                                " · Last used {format_added_date(last)}"
                            }
                        }
                    }
                }
            }
        }
        h2 { class: "notifications-section-title", "Signed-in sessions" }
        if sessions().is_empty() {
            p { class: "notifications-hint", "No other sessions on record." }
        } else {
            ul { class: "plugin-list",
                for row in sessions().iter().cloned() {
                    li {
                        class: "plugin-row",
                        key: "sess-{row.id}",
                        div {
                            class: "plugin-row-main",
                            span { class: "plugin-name",
                                "{format_session_method(&row.method)}"
                                if row.current {
                                    span { class: "lib-badge", "This browser" }
                                }
                            }
                            if !row.current {
                                div { class: "notifications-row-actions",
                                    SessionRevokeButton {
                                        api_base: list_base.clone(),
                                        public_id: row.id.clone(),
                                        on_message,
                                        on_refresh,
                                    }
                                }
                            }
                        }
                        if let Some(ua) = row.user_agent.as_ref() {
                            p { class: "plugin-meta", "{ua}" }
                        }
                        p { class: "plugin-meta",
                            "Last seen {format_added_date(&row.last_seen_at)}"
                        }
                    }
                }
            }
        }
    }
}
