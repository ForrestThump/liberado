//! Permission card rendered in the owning chat.
//!
//! Labels come from the wire payload. This module does not know Deny / Once / Session /
//! Everywhere. A test pins that: a card whose option label is `CUSTOM` renders `CUSTOM`.

use dioxus::prelude::*;

use chat_client_contract::{
    ApprovalCard, ApprovalListResponse, ApprovalResolveRequest, ApprovalResolveResponse,
};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct CardView {
    pub title: String,
    pub summary: String,
    pub options: Vec<(String, String)>,
    pub enabled: bool,
    pub banner: Option<String>,
}

/// What the card shows. Buttons use `options`. A decided card is disabled and says
/// `already decided: {decision_label}`.
pub(super) fn card_view(card: &ApprovalCard) -> CardView {
    let banner = if card.pending {
        None
    } else {
        Some(format!(
            "already decided: {}",
            card.decision_label.as_deref().unwrap_or("Decided")
        ))
    };
    CardView {
        title: card.title.clone(),
        summary: card.summary.clone(),
        options: card
            .options
            .iter()
            .map(|option| (option.action.clone(), option.label.clone()))
            .collect(),
        enabled: card.pending,
        banner,
    }
}

pub(super) fn apply_local(cards: &mut [ApprovalCard], id: &str, decision: String, label: String) {
    if let Some(card) = cards.iter_mut().find(|card| card.id == id) {
        card.pending = false;
        card.decision = Some(decision);
        card.decision_label = Some(label);
    }
}

pub(super) async fn fetch_approvals(
    api_base: &str,
    session: &str,
) -> Result<Vec<ApprovalCard>, String> {
    let url = format!("{api_base}/api/conversations/{session}/approvals");
    let body: ApprovalListResponse = reqwest::get(&url)
        .await
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    Ok(body.approvals)
}

/// `Ok` for 200 and 409. Both carry the decision that won.
pub(super) async fn post_decision(
    api_base: &str,
    id: &str,
    decision: &str,
) -> Result<(String, String), String> {
    let url = format!("{api_base}/api/approvals/{id}/resolve");
    let response = reqwest::Client::new()
        .post(&url)
        .json(&ApprovalResolveRequest {
            decision: decision.to_string(),
        })
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status().as_u16();
    let body: ApprovalResolveResponse = response.json().await.map_err(|error| error.to_string())?;
    if status == 200 || status == 409 {
        return Ok((
            body.decision.unwrap_or_else(|| decision.to_string()),
            body.label.unwrap_or_else(|| "Decided".into()),
        ));
    }
    Err(body
        .error
        .unwrap_or_else(|| format!("resolve failed (HTTP {status})")))
}

#[component]
pub(super) fn ApprovalCards(
    api_base: String,
    cards: Signal<Vec<ApprovalCard>>,
    session: Signal<Option<String>>,
) -> Element {
    // Refresh while this chat is open. The effect reads `session` so a switch restarts the
    // timer. It does not read `cards`, so a tap does not reset the interval.
    #[cfg(target_arch = "wasm32")]
    {
        let base = api_base.clone();
        use_effect(move || {
            poll::restart_poll(base.clone(), session(), cards);
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = session();
    }
    let list = cards.read().clone();
    rsx! {
        for card in list {
            ApprovalCardRow {
                key: "{card.id}",
                card: card.clone(),
                api_base: api_base.clone(),
                cards,
            }
        }
    }
}

#[component]
fn ApprovalCardRow(
    card: ApprovalCard,
    api_base: String,
    cards: Signal<Vec<ApprovalCard>>,
) -> Element {
    let view = card_view(&card);
    let id = card.id.clone();
    rsx! {
        div { class: "approval-card",
            p { class: "approval-title", "{view.title}" }
            p { class: "approval-summary", "{view.summary}" }
            if let Some(banner) = view.banner.clone() {
                p { class: "approval-decided", "{banner}" }
            }
            div { class: "approval-options",
                for (action, label) in view.options.iter() {
                    button {
                        class: "approval-option",
                        r#type: "button",
                        disabled: !view.enabled,
                        onclick: {
                            let action = action.clone();
                            let api_base = api_base.clone();
                            let id = id.clone();
                            move |_| tap(api_base.clone(), id.clone(), action.clone(), cards)
                        },
                        "{label}"
                    }
                }
            }
        }
    }
}

fn tap(api_base: String, id: String, action: String, cards: Signal<Vec<ApprovalCard>>) {
    #[cfg(target_arch = "wasm32")]
    {
        let mut cards = cards;
        wasm_bindgen_futures::spawn_local(async move {
            if let Ok((decision, label)) = post_decision(&api_base, &id, &action).await {
                cards.with_mut(|list| apply_local(list, &id, decision, label));
            }
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (api_base, id, action, cards);
    }
}

#[cfg(target_arch = "wasm32")]
mod poll {
    use std::cell::{Cell, RefCell};

    use super::*;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    thread_local! {
        static POLL_ID: Cell<Option<i32>> = const { Cell::new(None) };
        static POLL_CB: RefCell<Option<Closure<dyn FnMut()>>> = const { RefCell::new(None) };
    }

    pub(super) fn restart_poll(
        api_base: String,
        session_id: Option<String>,
        cards: Signal<Vec<ApprovalCard>>,
    ) {
        if let Some(id) = POLL_ID.take()
            && let Some(window) = web_sys::window()
        {
            window.clear_interval_with_handle(id);
        }
        POLL_CB.with(|cell| *cell.borrow_mut() = None);
        let Some(session_id) = session_id else {
            return;
        };
        let callback = Closure::wrap(Box::new(move || {
            let base = api_base.clone();
            let session = session_id.clone();
            let mut cards = cards;
            wasm_bindgen_futures::spawn_local(async move {
                if let Ok(next) = fetch_approvals(&base, &session).await {
                    cards.set(next);
                }
            });
        }) as Box<dyn FnMut()>);
        let Some(window) = web_sys::window() else {
            return;
        };
        let Ok(id) = window.set_interval_with_callback_and_timeout_and_arguments_0(
            callback.as_ref().unchecked_ref(),
            3_000,
        ) else {
            return;
        };
        POLL_ID.set(Some(id));
        POLL_CB.with(|cell| *cell.borrow_mut() = Some(callback));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(pending: bool, label: &str) -> ApprovalCard {
        ApprovalCard {
            id: "perm-1".into(),
            title: "Permission".into(),
            summary: "needs write".into(),
            options: vec![chat_client_contract::ApprovalOption {
                action: "once".into(),
                label: label.into(),
            }],
            pending,
            decision: (!pending).then(|| "once".into()),
            decision_label: (!pending).then(|| "Approved once".into()),
            created_at: "2026-10-10T00:00:00Z".into(),
        }
    }

    #[test]
    fn the_button_label_is_the_wire_label() {
        let view = card_view(&card(true, "CUSTOM"));
        assert_eq!(view.options, vec![("once".into(), "CUSTOM".into())]);
        assert!(view.enabled);
        assert!(view.banner.is_none());
    }

    #[test]
    fn a_decided_card_disables_the_buttons_and_names_the_decision() {
        let view = card_view(&card(false, "CUSTOM"));
        assert!(!view.enabled);
        assert_eq!(
            view.banner.as_deref(),
            Some("already decided: Approved once")
        );
    }

    #[test]
    fn a_local_tap_marks_the_matching_card_decided() {
        let mut cards = vec![card(true, "CUSTOM")];
        apply_local(&mut cards, "perm-1", "once".into(), "Approved once".into());
        assert!(!cards[0].pending);
        assert_eq!(cards[0].decision_label.as_deref(), Some("Approved once"));
        let view = card_view(&cards[0]);
        assert_eq!(
            view.banner.as_deref(),
            Some("already decided: Approved once")
        );
    }
}
