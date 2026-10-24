//! The Automations-page card. The permission request starts in the click.

use dioxus::prelude::*;

use super::actions;
use super::browser;
use super::model::{self, PushUi, ViewModel};

#[component]
pub fn NotificationsCard(api_base: String) -> Element {
    let status = use_resource({
        let base = api_base.clone();
        move || {
            let base = base.clone();
            async move { actions::fetch_status(&base).await }
        }
    });
    let endpoint = use_signal(String::new);
    let error = use_signal(|| None::<String>);
    let busy = use_signal(|| false);
    use_hook({
        let mut endpoint = endpoint;
        move || {
            spawn(async move {
                if let Some(found) = browser::existing_endpoint().await {
                    endpoint.set(found);
                }
            });
        }
    });
    let bits = browser::live_bits(!endpoint().is_empty());
    let model = match status.read().as_ref() {
        None => model::view_model(None, &bits),
        Some(Ok(server)) => model::view_model(Some(Ok(server)), &bits),
        Some(Err(err)) => model::view_model(Some(Err(err)), &bits),
    };
    let enable_base = api_base.clone();
    let unsub_base = api_base.clone();
    let test_base = api_base.clone();
    let enable_key = model.public_key.clone();

    rsx! {
        section {
            class: "push-card",
            h2 { class: "push-card-title", "Notifications" }
            p { class: "push-card-note", "{model.sentence}" }
            if !model.reminder.is_empty() {
                p { class: "push-card-note", "{model.reminder}" }
            }
            if let Some(message) = error() {
                p { class: "push-card-note", "{message}" }
            }
            div {
                class: "push-card-actions",
                if show_enable(&model) {
                    button {
                        class: "approval-option",
                        r#type: "button",
                        disabled: busy(),
                        onclick: move |_| {
                            browser::begin_enable(
                                enable_base.clone(),
                                enable_key.clone(),
                                endpoint,
                                error,
                                busy,
                            );
                        },
                        "Enable notifications"
                    }
                }
                if show_subscribed(&model) {
                    button {
                        class: "approval-option",
                        r#type: "button",
                        disabled: busy(),
                        onclick: move |_| {
                            actions::begin_unsubscribe(unsub_base.clone(), endpoint, error, busy)
                        },
                        "Unsubscribe"
                    }
                    button {
                        class: "approval-option",
                        r#type: "button",
                        disabled: busy(),
                        onclick: move |_| {
                            actions::begin_test(test_base.clone(), endpoint, error, busy)
                        },
                        "Send test notification"
                    }
                }
            }
        }
    }
}

fn show_enable(model: &ViewModel) -> bool {
    model.ui == PushUi::Ready && !model.public_key.is_empty()
}

fn show_subscribed(model: &ViewModel) -> bool {
    model.ui == PushUi::Subscribed
}
