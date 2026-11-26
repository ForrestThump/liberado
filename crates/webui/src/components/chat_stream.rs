//! The live turn: one SSE connection to the daemon, and what it writes.
//!
//! A turn opens in the browser and runs to the end of the reply. The stream
//! reports the conversation it opened, appends the tokens of the reply, records
//! the tool calls, and closes when the turn is done. All of that is this
//! module's job, and it is why the stream lives here rather than in the chat
//! surface: the surface renders, this one talks to the daemon.
//!
//! The transcript pin is the one thing here that is not the stream's business,
//! and it is the one thing the stream must get right. The conversation a turn
//! opens is the conversation the reader's own send opened, and the refresh at
//! the end of that turn must not take the bottom of the pane back from a reader
//! who scrolled up while the reply was streaming. The rule lives in
//! `chat_scroll`; the stream records the event the rule needs.

use dioxus::prelude::*;

use super::chat_fold;
use super::chat_input::{stream_url, truncate_title};
use super::chat_reasoning;
use super::chat_tool_events;
use super::{ChatMsg, ThinkingStep, fetch_conversation};
use crate::signal_alive::set_if_alive;

thread_local! {
    static CURRENT_SOURCE: std::cell::RefCell<Option<std::rc::Rc<web_sys::EventSource>>> =
        const { std::cell::RefCell::new(None) };
}

pub(super) fn close_current_stream() {
    CURRENT_SOURCE.with(|cell| {
        if let Some(s) = cell.borrow_mut().take() {
            s.close();
        }
    });
}

/// The chat state one live turn writes into.
///
/// Bundled rather than passed one-by-one: `open_stream` was already carrying five signals plus two
/// base URLs, and incognito wanted two more. Signals are `Copy`, so this is a plain regrouping with
/// no lifetime or ownership consequences — the SSE closures below each still capture only what they
/// touch.
#[derive(Clone, Copy)]
pub(super) struct StreamTargets {
    pub(super) messages: Signal<Vec<ChatMsg>>,
    pub(super) sending: Signal<bool>,
    pub(super) session: Signal<Option<String>>,
    /// Left untouched by an incognito turn — that session is not in the sidebar to be selected.
    pub(super) active_conv_id: Signal<Option<String>>,
    pub(super) should_set_title: Signal<bool>,
    /// Where an incognito turn records the session it opened, for the teardown paths.
    pub(super) ghost_session: Signal<Option<String>>,
    /// The transcript pin. The conversation the reader's own send opened is recorded
    /// here, so the refresh at the end of that turn is not read as a new open.
    pub(super) follow: Signal<super::chat_scroll::TranscriptStick>,
    /// Bumped when `create_agent` succeeds. The shelf reads it and refetches.
    pub(super) list_epoch: Signal<u64>,
}

pub(super) struct StreamTurnOptions {
    pub(super) profile: Option<String>,
    pub(super) model: Option<String>,
    pub(super) project: Option<String>,
}

pub(super) fn open_stream(
    api_base: &str,
    message: &str,
    targets: StreamTargets,
    api_base_for_title: &str,
    incognito: bool,
    turn: StreamTurnOptions,
) {
    let StreamTurnOptions {
        profile: pending_profile,
        model: pending_model,
        project: pending_project,
    } = turn;
    let encoded = urlencoding::encode(message);
    let url = stream_url(
        api_base,
        &encoded,
        targets.session.read().as_deref(),
        incognito,
        pending_profile.as_deref(),
        pending_model.as_deref(),
    );
    // **Whether this turn is opening a private session**, which is not the same question as whether
    // the mode is on. If `session` is already set we are continuing an existing conversation, and no
    // flag can retroactively make that one private. Conflating the two once destroyed a saved chat.
    let opened_incognito = incognito && targets.session.read().is_none();
    // A send owns its turn from the start, so a dropped stream means what it says.
    connect_stream(
        &url,
        api_base,
        targets,
        api_base_for_title,
        opened_incognito,
        None,
        pending_project,
    );
}

/// Rejoin a turn already running for `conv_id`, without sending anything.
///
/// The reload path. The daemon replays what has already happened and then continues live, so the
/// answer being written while the page was gone still lands on screen. Never opens an incognito
/// session: attaching is by definition to a conversation that already exists.
pub(super) fn attach_stream(
    api_base: &str,
    conv_id: &str,
    targets: StreamTargets,
    api_base_for_title: &str,
) {
    let url = format!("{api_base}/api/conversations/{conv_id}/attach");
    connect_stream(
        &url,
        api_base,
        targets,
        api_base_for_title,
        false,
        Some(conv_id.to_string()),
        None,
    );
}

/// Open an SSE connection at `url` and wire it into `targets`. Shared by the send path and the
/// reattach path, so a turn renders identically however this client came to be watching it.
fn connect_stream(
    url: &str,
    api_base: &str,
    targets: StreamTargets,
    api_base_for_title: &str,
    opened_incognito: bool,
    // Conversation to re-read from the store if this stream ends badly. Set for an *attach*, where
    // the answer may already be on disk and the stream failing says nothing about the turn.
    reconcile: Option<String>,
    pending_project: Option<String>,
) {
    let StreamTargets {
        mut messages,
        mut sending,
        mut session,
        mut active_conv_id,
        mut should_set_title,
        mut ghost_session,
        mut follow,
        list_epoch,
    } = targets;
    use std::rc::Rc;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;
    use web_sys::{EventSource, MessageEvent};

    let ghost_base = api_base.to_string();
    let source = match EventSource::new(url) {
        Ok(s) => Rc::new(s),
        Err(_) => {
            sending.set(false);
            return;
        }
    };
    CURRENT_SOURCE.with(|cell| {
        *cell.borrow_mut() = Some(source.clone());
    });

    // session -> record it, and (for a normal chat) set active_conv_id so the sidebar highlights it.
    {
        let on_session = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            if let Some(data) = e.data().as_string()
                && !data.is_empty()
            {
                session.set(Some(data.clone()));
                // The conversation this turn is in. The refresh at the end of the turn is
                // for it, and it must not take the bottom back from a reader who scrolled
                // up while the reply streamed.
                follow.write().note_sent_conversation(&data);
                if opened_incognito {
                    // Recorded as the ghost instead of the active conversation: it is not in the
                    // sidebar to be highlighted, and this is the id the teardown paths need. The
                    // `pagehide` mirror is set here too — this is the first moment there is anything
                    // to discard.
                    crate::components::incognito::remember(&ghost_base, &data);
                    ghost_session.set(Some(data));
                } else {
                    active_conv_id.set(Some(data));
                }
            }
        });
        let _ =
            source.add_event_listener_with_callback("session", on_session.as_ref().unchecked_ref());
        on_session.forget();
    }

    // token -> append delta to the last assistant message, creating one on first sight.
    {
        let on_token = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            if let Some(data) = e.data().as_string() {
                messages.with_mut(|m| match m.last_mut() {
                    Some(last) if last.role == "assistant" => {
                        chat_reasoning::append_token(last, &data);
                    }
                    _ => {
                        let mut created = ChatMsg::new_assistant(String::new());
                        chat_reasoning::append_token(&mut created, &data);
                        m.push(created);
                    }
                });
            }
        });
        let _ = source.add_event_listener_with_callback("token", on_token.as_ref().unchecked_ref());
        on_token.forget();
    }

    // tool_started -> append a pending ThinkingStep to the last assistant message (creating one if
    // needed). Payload decoding goes through the shared converged vocabulary
    // (chat_client_contract::SessionEvent — same decoder as the TUI/CLI).
    {
        let on_tool = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            if let Some(data) = e.data().as_string()
                && let Ok(chat_client_contract::SessionEvent {
                    kind: chat_client_contract::SessionEventKind::ToolStarted { name, args_preview },
                    ..
                }) = chat_client_contract::SessionEvent::from_sse_data("tool_started", &data)
            {
                let clean = chat_fold::clean_args(&args_preview);
                messages.with_mut(|m| match m.last_mut() {
                    Some(last) if last.role == "assistant" => {
                        last.thinking_steps.push(ThinkingStep {
                            tool_name: name,
                            tool_args: clean.clone(),
                            ok: None,
                            preview: String::new(),
                        });
                    }
                    _ => {
                        let mut msg = ChatMsg::new_assistant(String::new());
                        msg.thinking_steps.push(ThinkingStep {
                            tool_name: name,
                            tool_args: clean,
                            ok: None,
                            preview: String::new(),
                        });
                        m.push(msg);
                    }
                });
            }
        });
        let _ = source
            .add_event_listener_with_callback("tool_started", on_tool.as_ref().unchecked_ref());
        on_tool.forget();
    }

    // tool_finished -> close the pending step, and refetch the shelf when an agent was created.
    {
        let on_result = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            if let Some(data) = e.data().as_string() {
                chat_tool_events::note_tool_finished(&data, messages, list_epoch);
            }
        });
        let _ = source
            .add_event_listener_with_callback("tool_finished", on_result.as_ref().unchecked_ref());
        on_result.forget();
    }

    // session_finished -> close + stop + optionally set title.
    {
        let source_done = source.clone();
        let title_base = api_base_for_title.to_string();
        let on_done = Closure::<dyn FnMut(MessageEvent)>::new(move |_e: MessageEvent| {
            chat_reasoning::adopt_when_finished(title_base.clone(), session, messages);
            source_done.close();
            sending.set(false);
            CURRENT_SOURCE.with(|cell| *cell.borrow_mut() = None);

            // Skipped for incognito: a title exists to label a row in the sidebar, and an incognito
            // session has no row. Naming it would only mean sending the first thing the user typed
            // back over the wire for a field nobody will ever read.
            //
            // Keyed on `opened_incognito`, not the mode flag — the rule this file already states
            // elsewhere. With the mode armed but a *saved* chat open, that chat does have a row and
            // does want a title; reading the raw flag left it permanently untitled.
            if should_set_title() && !opened_incognito {
                should_set_title.set(false);
                let title_opt = messages
                    .read()
                    .iter()
                    .find(|m| m.role == "user")
                    .map(|m| truncate_title(&m.content));
                let conv_id_opt = session.read().clone();
                if let (Some(title), Some(conv_id)) = (title_opt, conv_id_opt) {
                    let base = title_base.clone();
                    let project_for_title = pending_project.clone();
                    web_sys::console::log_1(
                        &format!("[title] setting title for {}: {}", conv_id, title).into(),
                    );
                    wasm_bindgen_futures::spawn_local(async move {
                        let url = format!("{base}/api/conversations/{conv_id}");
                        let body = crate::components::project_data::title_patch_body(
                            &title,
                            project_for_title.as_deref(),
                        );
                        match crate::http::patch_json(&url, &body).await {
                            Ok(resp) => {
                                if !resp.status().is_success() {
                                    web_sys::console::log_1(
                                        &format!(
                                            "[title] PATCH failed: {} - {}",
                                            resp.status(),
                                            resp.text().await.unwrap_or_default()
                                        )
                                        .into(),
                                    );
                                }
                            }
                            Err(e) => {
                                web_sys::console::log_1(
                                    &format!("[title] PATCH error: {}", e).into(),
                                );
                            }
                        }
                    });
                } else {
                    web_sys::console::log_1(
                        &"[title] should_set_title was true but no user message or session".into(),
                    );
                }
            }
        });
        let _ = source
            .add_event_listener_with_callback("session_finished", on_done.as_ref().unchecked_ref());
        on_done.forget();
    }

    // failed -> replace in-flight message with error, close + stop. Payload is JSON {message}
    // (converged vocabulary); fall back to the raw data if it isn't.
    {
        let source_fail = source.clone();
        let on_fail = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            let raw = e
                .data()
                .as_string()
                .unwrap_or_else(|| "stream error".into());
            let msg = match chat_client_contract::SessionEvent::from_sse_data("failed", &raw) {
                Ok(chat_client_contract::SessionEvent {
                    kind: chat_client_contract::SessionEventKind::Failed { message },
                    ..
                }) => message,
                _ => raw,
            };
            messages.with_mut(|m| m.push(ChatMsg::new_error(msg)));
            source_fail.close();
            sending.set(false);
            CURRENT_SOURCE.with(|cell| *cell.borrow_mut() = None);
        });
        let _ = source.add_event_listener_with_callback("failed", on_fail.as_ref().unchecked_ref());
        on_fail.forget();
    }

    // Native EventSource error (connection dropped, or the attach was refused).
    //
    // For an attach this is not evidence about the turn. The commonest cause is a race the client
    // cannot avoid: the history said a turn was running, it finished before the attach arrived, and
    // the daemon answered 409 because there is nothing left to join. The answer is already on disk.
    // Ending quietly here would show the question with no reply — the exact silence this change set
    // exists to remove — so ask the store rather than guess.
    {
        let source_err = source.clone();
        let reconcile_err = reconcile.clone();
        let reconcile_base = api_base.to_string();
        let on_err = Closure::<dyn FnMut(MessageEvent)>::new(move |_e: MessageEvent| {
            source_err.close();
            sending.set(false);
            CURRENT_SOURCE.with(|cell| *cell.borrow_mut() = None);
            if let Some(conv_id) = reconcile_err.clone() {
                let base = reconcile_base.clone();
                let mut messages = messages;
                wasm_bindgen_futures::spawn_local(async move {
                    if let Ok(loaded) = fetch_conversation(&base, &conv_id).await {
                        let _ = set_if_alive(&mut messages, loaded.messages);
                    }
                });
            }
        });
        source.set_onerror(Some(on_err.as_ref().unchecked_ref()));
        on_err.forget();
    }
}
