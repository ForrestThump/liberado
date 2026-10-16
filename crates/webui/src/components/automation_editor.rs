//! Automation Editor dock under the schedule list.
//!
//! One mount posts the singleton open. The transcript reuses the chat message
//! rows. Approve and Deny are the shared approval cards for this session id.
//! The composer posts `POST /api/chat`. A short poll reloads the thread so the
//! note after Approve — the file is on disk, or the write was refused — shows
//! in the transcript. The transcript follows the bottom only while the reader
//! is already there, or just after they send.

use dioxus::prelude::*;

use chat_client_contract::ApprovalCard;

use super::approval_card::ApprovalCards;
use super::chat::ChatMsg;
use super::message_row::MessageList;

#[path = "automation_editor_logic.rs"]
mod logic;

#[path = "automation_editor_net.rs"]
mod net;

#[cfg(target_arch = "wasm32")]
#[path = "automation_editor_poll.rs"]
mod poll;

#[cfg(target_arch = "wasm32")]
use logic::SCROLL_TRIGGER_SEND;
use logic::{assistant_line, begin_send, dock_class, send_is_disabled, send_label};

#[derive(Clone, Copy)]
struct EditorUi {
    session: Signal<Option<String>>,
    messages: Signal<Vec<ChatMsg>>,
    pending_ids: Signal<Vec<String>>,
    draft: Signal<String>,
    sending: Signal<bool>,
    status: Signal<Option<String>>,
    /// True while the reader is near the bottom, or just after they send.
    follow: Signal<bool>,
}

#[component]
pub fn AutomationEditor(api_base: String) -> Element {
    let session = use_signal(|| None::<String>);
    let messages = use_signal(Vec::new);
    let cards = use_signal(Vec::<ApprovalCard>::new);
    let mut draft = use_signal(String::new);
    let sending = use_signal(|| false);
    let status = use_signal(|| None::<String>);
    let pending_ids = use_signal(Vec::<String>::new);
    let follow = use_signal(|| true);
    let ui = EditorUi {
        session,
        messages,
        pending_ids,
        draft,
        sending,
        status,
        follow,
    };

    mount_editor(api_base.clone(), ui);

    let send_disabled = send_is_disabled(sending(), session().is_none());
    let label = send_label(sending());
    let class = dock_class(pending_ids().is_empty());
    let base = api_base.clone();

    rsx! {
        section {
            class: "{class}",
            aria_label: "Automation Editor",
            div {
                class: "automation-editor-split",
                role: "separator",
            }
            div {
                class: "automation-editor-transcript",
                id: "automation-editor-transcript",
                MessageList { messages }
                ApprovalCards {
                    api_base: api_base.clone(),
                    cards,
                    session,
                }
            }
            form {
                class: "automation-editor-composer",
                onsubmit: move |evt| {
                    evt.prevent_default();
                    submit_editor_message(base.clone(), ui);
                },
                textarea {
                    class: "automation-editor-input",
                    rows: "2",
                    placeholder: "Ask the editor to change a schedule",
                    aria_label: "Message the Automation Editor",
                    value: "{draft}",
                    disabled: sending(),
                    oninput: move |evt| draft.set(evt.value()),
                }
                button {
                    class: "automation-editor-send",
                    r#type: "submit",
                    disabled: send_disabled,
                    "{label}"
                }
            }
            EditorStatus { message: status() }
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
fn mount_editor(api_base: String, mut ui: EditorUi) {
    #[cfg(target_arch = "wasm32")]
    {
        let mut started = use_signal(|| false);
        let base = api_base.clone();
        use_effect(move || {
            if started() {
                return;
            }
            started.set(true);
            let base = base.clone();
            wasm_bindgen_futures::spawn_local(async move {
                match net::open_editor(&base).await {
                    Ok(id) => {
                        ui.session.set(Some(id.clone()));
                        apply_snapshot(&base, &id, ui).await;
                    }
                    Err(message) => ui.status.set(Some(message)),
                }
            });
        });
        let base = api_base.clone();
        use_effect(move || {
            poll::restart(base.clone(), (ui.session)(), ui.messages, ui.pending_ids);
        });
        use_effect(move || {
            let _count = ui.messages.read().len();
            let why = poll::take_scroll_why();
            let stick = *ui.follow.peek();
            poll::scroll_transcript(stick, why);
        });
        use_effect(move || {
            poll::watch_editor_scroll(ui.follow);
        });
        use_drop(poll::stop);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (api_base, ui);
    }
}

async fn apply_snapshot(api_base: &str, session_id: &str, mut ui: EditorUi) {
    match net::load_snapshot(api_base, session_id).await {
        Ok(snapshot) => {
            ui.messages.set(snapshot.messages);
            ui.pending_ids.set(snapshot.pending_ids);
        }
        Err(message) => ui.status.set(Some(message)),
    }
}

fn submit_editor_message(api_base: String, mut ui: EditorUi) {
    let Some(text) = begin_send(
        (ui.sending)(),
        ui.session.read().is_some(),
        &ui.draft.read(),
    ) else {
        return;
    };
    let Some(session_id) = ui.session.read().clone() else {
        return;
    };
    ui.draft.set(String::new());
    ui.sending.set(true);
    ui.status.set(None);
    ui.follow.set(true);
    #[cfg(target_arch = "wasm32")]
    poll::scroll_transcript(true, SCROLL_TRIGGER_SEND);
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        match net::post_turn(&api_base, &session_id, &text).await {
            Ok(reply) => finish_turn(&api_base, &session_id, reply, ui).await,
            Err(message) => ui.status.set(Some(message)),
        }
        ui.sending.set(false);
    });
    #[cfg(not(target_arch = "wasm32"))]
    {
        ui.sending.set(false);
        let _ = (api_base, text, session_id);
    }
}

async fn finish_turn(api_base: &str, session_id: &str, reply: String, mut ui: EditorUi) {
    match net::load_snapshot(api_base, session_id).await {
        Ok(snapshot) => {
            ui.messages.set(snapshot.messages);
            ui.pending_ids.set(snapshot.pending_ids);
            ui.status.set(None);
        }
        Err(_) => {
            ui.messages.write().push(assistant_line(reply));
        }
    }
}

#[component]
fn EditorStatus(message: Option<String>) -> Element {
    let Some(message) = message else {
        return rsx! {};
    };
    rsx! {
        p { class: "automation-editor-status", "{message}" }
    }
}
