//! Owner notifications — Web Push subscriptions and outbox visibility.

mod api;
mod model;

use dioxus::prelude::*;

use self::api::{
    RefreshBundle, enable_push, refresh_and_reconcile, remove_subscription, send_test,
    turn_off_push,
};
use self::model::{
    DevicePushState, DevicePushStatus, PushStatus, StatusLine, SubscriptionRow,
    display_push_status, format_added_date, push_test_success_message, status_pill_class,
    status_pill_label, troubleshooting_lines,
};

fn apply_refresh_bundle(
    status: &mut Signal<PushStatus>,
    subs: &mut Signal<Vec<SubscriptionRow>>,
    device_state: &mut Signal<DevicePushState>,
    line: &mut Signal<Option<StatusLine>>,
    bundle: RefreshBundle,
) {
    status.set(bundle.status);
    subs.set(bundle.subs);
    device_state.set(bundle.device);
    if let Some(err) = bundle.reconcile_error {
        line.set(Some(StatusLine::Error(err)));
    }
}

#[component]
fn RemoveSubscriptionButton(
    api_base: String,
    endpoint: String,
    on_done: EventHandler<Result<(PushStatus, Vec<SubscriptionRow>, DevicePushState), String>>,
    on_error: EventHandler<String>,
) -> Element {
    rsx! {
        button {
            class: "lib-btn danger",
            r#type: "button",
            onclick: move |_| {
                let base = api_base.clone();
                let endpoint = endpoint.clone();
                spawn(async move {
                    if let Err(err) = remove_subscription(&base, &endpoint).await {
                        on_error.call(err);
                    } else if let Ok(bundle) = refresh_and_reconcile(&base).await {
                        on_done.call(Ok((bundle.status, bundle.subs, bundle.device)));
                    }
                });
            },
            "Remove"
        }
    }
}

#[component]
pub fn Notifications(api_base: String) -> Element {
    let mut status = use_signal(PushStatus::default);
    let mut subs = use_signal(Vec::<SubscriptionRow>::new);
    let mut device_state = use_signal(DevicePushState::default);
    let mut line = use_signal(|| None::<StatusLine>);
    let base = api_base.clone();

    use_effect(move || {
        let base = base.clone();
        spawn(async move {
            match refresh_and_reconcile(&base).await {
                Ok(bundle) => apply_refresh_bundle(
                    &mut status,
                    &mut subs,
                    &mut device_state,
                    &mut line,
                    bundle,
                ),
                Err(err) => line.set(Some(StatusLine::Error(err))),
            }
        });
    });

    let push_status = display_push_status(&device_state(), status().vapid_configured, &subs());
    let this_endpoint = device_state().subscription_endpoint.clone();
    let badge_endpoint = this_endpoint.clone();
    let ios_hint = "Add this site to your Home Screen (Share → Add to Home Screen), open it from the icon, then turn notifications on here.";
    let show_ios_hint = device_state().is_ios
        && !device_state().display_standalone
        && push_status != DevicePushStatus::On;

    let enable_base = api_base.clone();
    let test_base = api_base.clone();
    let off_base = api_base.clone();
    let list_base = api_base.clone();

    rsx! {
        div {
            class: "grok-page notifications-page",
            div {
                class: "grok-page-header",
                div { class: "grok-page-eyebrow", "Settings" }
                h1 { class: "grok-page-title", "Notifications" }
                p { class: "grok-page-subtitle", "Get reminders on this device." }
            }
            div {
                class: "grok-page-body",
                if let Some(msg) = line() {
                    match msg {
                        StatusLine::Error(text) => rsx! {
                            p { class: "notifications-inline notifications-inline-error", "{text}" }
                        },
                        StatusLine::Success(text) => rsx! {
                            p { class: "notifications-inline notifications-inline-success", "{text}" }
                        },
                    }
                }
                if push_status == DevicePushStatus::Blocked {
                    p { class: "notifications-inline notifications-inline-error",
                        "Notifications are blocked. Allow them in browser site settings, then try again."
                    }
                }
                if show_ios_hint {
                    p { class: "notifications-hint", "{ios_hint}" }
                }
                div {
                    class: "card notifications-device-card",
                    div {
                        class: "card-header",
                        h2 { "This device" }
                    }
                    div {
                        class: "card-body notifications-device-body",
                        span { class: "{status_pill_class(push_status)}", "{status_pill_label(push_status)}" }
                        div { class: "notifications-device-actions",
                            if push_status == DevicePushStatus::Off {
                                button {
                                    class: "lib-btn primary",
                                    r#type: "button",
                                    onclick: move |_| {
                                        let base = enable_base.clone();
                                        spawn(async move {
                                            match enable_push(&base).await {
                                                Ok(()) => {
                                                    line.set(Some(StatusLine::Success(
                                                        "Push enabled on this device.".into(),
                                                    )));
                                                    if let Ok(bundle) = refresh_and_reconcile(&base).await {
                                                        apply_refresh_bundle(
                                                            &mut status,
                                                            &mut subs,
                                                            &mut device_state,
                                                            &mut line,
                                                            bundle,
                                                        );
                                                    }
                                                }
                                                Err(err) => line.set(Some(StatusLine::Error(err))),
                                            }
                                        });
                                    },
                                    "Turn on"
                                }
                            }
                            if push_status == DevicePushStatus::On {
                                button {
                                    class: "lib-btn primary",
                                    r#type: "button",
                                    onclick: move |_| {
                                        let base = test_base.clone();
                                        let endpoint = this_endpoint.clone();
                                        spawn(async move {
                                            match send_test(&base, &endpoint).await {
                                                Ok(()) => line.set(Some(StatusLine::Success(
                                                    push_test_success_message().into(),
                                                ))),
                                                Err(err) => {
                                                    line.set(Some(StatusLine::Error(err)));
                                                    if let Ok(bundle) = refresh_and_reconcile(&base).await {
                                                        apply_refresh_bundle(
                                                            &mut status,
                                                            &mut subs,
                                                            &mut device_state,
                                                            &mut line,
                                                            bundle,
                                                        );
                                                    }
                                                }
                                            }
                                        });
                                    },
                                    "Send test"
                                }
                                button {
                                    class: "lib-btn link",
                                    r#type: "button",
                                    onclick: move |_| {
                                        let base = off_base.clone();
                                        spawn(async move {
                                            match turn_off_push(&base).await {
                                                Ok(()) => {
                                                    line.set(Some(StatusLine::Success(
                                                        "Push turned off on this device.".into(),
                                                    )));
                                                    if let Ok(bundle) = refresh_and_reconcile(&base).await {
                                                        apply_refresh_bundle(
                                                            &mut status,
                                                            &mut subs,
                                                            &mut device_state,
                                                            &mut line,
                                                            bundle,
                                                        );
                                                    }
                                                }
                                                Err(err) => line.set(Some(StatusLine::Error(err))),
                                            }
                                        });
                                    },
                                    "Turn off"
                                }
                            }
                        }
                    }
                }
                details { class: "notifications-troubleshooting",
                    summary { "Troubleshooting" }
                    dl { class: "notifications-diagnostics",
                        for (label, value) in troubleshooting_lines(&device_state(), &status()) {
                            dt { "{label}" }
                            dd { "{value}" }
                        }
                    }
                }
                if !subs().is_empty() {
                    h2 { class: "notifications-section-title", "Devices" }
                    ul { class: "plugin-list",
                        for row in subs().iter().cloned() {
                            li {
                                class: "plugin-row",
                                key: "{row.id}",
                                div {
                                    class: "plugin-row-main",
                                    span { class: "plugin-name", "{row.label}" }
                                    div { class: "notifications-row-actions",
                                        if !badge_endpoint.is_empty() && row.endpoint == badge_endpoint {
                                            span { class: "lib-badge", "This device" }
                                        }
                                        RemoveSubscriptionButton {
                                            api_base: list_base.clone(),
                                            endpoint: row.endpoint.clone(),
                                            on_done: move |result: Result<(PushStatus, Vec<SubscriptionRow>, DevicePushState), String>| {
                                                if let Ok((row, rows, device)) = result {
                                                    status.set(row);
                                                    subs.set(rows);
                                                    device_state.set(device);
                                                }
                                            },
                                            on_error: move |err: String| line.set(Some(StatusLine::Error(err))),
                                        }
                                    }
                                }
                                if !row.created_at.is_empty() {
                                    p { class: "plugin-meta",
                                        "Added {format_added_date(&row.created_at)}"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
