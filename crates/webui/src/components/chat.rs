use dioxus::prelude::*;

use chat_client_contract::ChatMessage;

use crate::components::markdown::MarkdownText;
use crate::components::model_browser::ModelBrowser;
use crate::components::picker::Picker;

// Slash commands only run in the browser — `submit` gates the whole block on wasm32, so gate the
// imports identically or a native build trips the workspace's zero-warnings bar on unused imports.
#[cfg(target_arch = "wasm32")]
use crate::components::slash_commands::handle_slash_command;
#[cfg(target_arch = "wasm32")]
use liberado_commands::CommandResult;

// ── Data types ──────────────────────────────────────────────────────────────

/// A tool call + its result, grouped as a thinking step under an assistant message.
#[derive(Clone, PartialEq)]
pub struct ThinkingStep {
    pub tool_name: String,
    pub tool_args: String,
    pub ok: Option<bool>, // None = still running, Some(true) = success, Some(false) = fail
    pub preview: String,
}

/// One rendered chat message.
#[derive(Clone, PartialEq)]
pub struct ChatMsg {
    pub role: &'static str, // "user" | "assistant" | "tool" | "system" | "error"
    pub content: String,
    /// Tool calls that happened during this assistant turn. Only populated during
    /// live SSE streaming; historical messages loaded from the API have empty vecs.
    pub thinking_steps: Vec<ThinkingStep>,
}

impl ChatMsg {
    fn new_user(content: String) -> Self {
        ChatMsg {
            role: "user",
            content,
            thinking_steps: Vec::new(),
        }
    }

    fn new_assistant(content: String) -> Self {
        ChatMsg {
            role: "assistant",
            content,
            thinking_steps: Vec::new(),
        }
    }

    fn new_error(content: String) -> Self {
        ChatMsg {
            role: "error",
            content,
            thinking_steps: Vec::new(),
        }
    }

    fn from_wire(msg: &ChatMessage) -> Self {
        let role = match msg.role.as_str() {
            "assistant" => "assistant",
            "tool" => "tool",
            "system" => "system",
            _ => "user",
        };
        ChatMsg {
            role,
            content: msg.content.clone(),
            thinking_steps: Vec::new(),
        }
    }
}

// ── History loading ─────────────────────────────────────────────────────────

async fn fetch_conversation(api_base: &str, conv_id: &str) -> Result<Vec<ChatMsg>, String> {
    let url = format!("{api_base}/api/conversations/{conv_id}");
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    let history: chat_client_contract::ConversationHistoryResponse = resp
        .json()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    // Drop `system` messages: in stored history that is the face agent's prompt, ~2.2k characters
    // of instructions that were being rendered as the opening chat bubble of every conversation.
    //
    // Filtered HERE and not in the renderer on purpose. The UI also builds `system` messages of its
    // own for slash-command output (`/help`, command errors — see components/slash_commands.rs), and
    // those must keep rendering. The distinction is provenance, not role, so it belongs at the point
    // the wire is decoded.
    Ok(history
        .messages
        .iter()
        .filter(|m| m.role != "system")
        .map(ChatMsg::from_wire)
        .collect())
}

// ── Chat component ──────────────────────────────────────────────────────────

#[component]
pub fn Chat(
    api_base: String,
    mut active_conv_id: Signal<Option<String>>,
    theme_name: Signal<String>,
    /// Incognito mode (see `components/incognito.rs`): the next chat opened is RAM-only on the
    /// daemon and discarded when it is left.
    incognito: Signal<bool>,
) -> Element {
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut theme_name = theme_name;
    // Written back when a sidebar pick takes the user out of a private chat — the mode has to follow
    // them out, or the banner ends up sitting above a conversation that is being written to disk.
    let mut incognito = incognito;
    // The live incognito session, if one has been opened. Tracked separately from `session` because
    // it is the thing that has to be *discarded*, and by the time we notice we are leaving, `session`
    // has often already moved on to whatever the user switched to.
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut ghost_session = use_signal(|| None::<String>);
    let mut messages = use_signal(Vec::new);
    let mut input = use_signal(String::new);
    let mut sending = use_signal(|| false);
    // `mut` is required by the wasm-only slash-command block below, which reassigns this. On a
    // native build those call sites are cfg'd out and the binding only looks immutable.
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut session = use_signal(|| None::<String>);
    let mut should_set_title = use_signal(|| false);
    // `/model` asks for a picker; this is it. Owned here because the command that opens it is
    // dispatched from `submit` below — and that dispatch is wasm-only, so on a native build the
    // only writer is cfg'd out and the binding merely looks immutable.
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut model_browser_open = use_signal(|| false);
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut theme_browser_open = use_signal(|| false);

    let base_for_effect = api_base.clone();
    let base_for_submit = api_base.clone();
    let base_for_title = api_base.clone();
    let base_for_slash = api_base.clone();
    let base_for_models = api_base.clone();

    use_effect(move || {
        if sending() {
            return;
        }
        let id = active_conv_id.read().clone();
        let base = base_for_effect.clone();
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(ref conv_id) = id {
                session.set(Some(conv_id.clone()));
                let conv_id = conv_id.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    if let Ok(msgs) = fetch_conversation(&base, &conv_id).await {
                        messages.set(msgs);
                    }
                });
            } else if ghost_session.read().is_none() {
                messages.set(Vec::new());
                session.set(None);
            }
            // An incognito chat is deliberately *not* an `active_conv_id`: it is not in the sidebar,
            // so there is nothing there to highlight, and letting it set one would make the
            // conversation list flicker at a row that does not exist. That leaves this effect seeing
            // `None` and reading it as "fresh chat" — which would blank the live transcript the
            // moment `sending` flips false at the end of the first turn. The guard above is what
            // keeps a private chat on screen; `discard_ghost` is the only thing that clears it.
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = &id;
            let _ = &base;
        }
    });

    // Tell the daemon to drop the live incognito session. Deliberately does *not* touch `messages`
    // or `session`: the three callers below want different things there, and one of them is about to
    // load a different conversation into both. Idempotent — with nothing live it does nothing.
    let base_for_discard = api_base.clone();
    let discard_ghost = use_callback(move |_: ()| {
        let Some(id) = ghost_session.write().take() else {
            return;
        };
        crate::components::incognito::forget();
        let base = base_for_discard.clone();
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(crate::components::incognito::discard(base, id));
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (base, id);
        }
    });

    // The whole lifecycle of the mode, in one effect, so the ways in and out cannot disagree with
    // each other. `prev` is what makes the *transitions* visible: an effect only ever sees the
    // current value, and "incognito is on" and "incognito just came on" call for different things.
    let mut prev_incognito = use_signal(|| false);
    use_effect(move || {
        let now = incognito();
        let was = prev_incognito();

        if now != was {
            prev_incognito.set(now);
            discard_ghost.call(());
            // Entering means a *new* chat, always. Without this, switching the mode on while a saved
            // conversation was open would leave `session` pointing at it — and the next message
            // would quietly continue that durable conversation with the banner promising privacy,
            // which is the worst failure this feature could have.
            messages.set(Vec::new());
            session.set(None);
            if active_conv_id.read().is_some() {
                active_conv_id.set(None);
            }
            if now {
                // Only once the mode has actually been used: no reason to hold an unload handler for
                // a window that has never opened a private chat.
                crate::components::incognito::install_unload_discard();
            }
            return;
        }

        // Still on, and a conversation got selected in the sidebar — including "New Chat", which
        // sets `None` and is still a request to be somewhere else. Leave the private chat and let
        // the mode follow, so the banner never sits above a conversation that is being written to
        // disk. `messages`/`session` are left alone here: the history effect above owns them now.
        if now && ghost_session.read().is_some() && active_conv_id.read().is_some() {
            discard_ghost.call(());
            incognito.set(false);
            prev_incognito.set(false);
        }
    });

    let mut stop_stream = move || {
        #[cfg(target_arch = "wasm32")]
        crate::components::chat::close_current_stream();
        sending.set(false);
    };

    // `use_callback` (not a plain closure) so the same handle is `Copy` and can be moved into both
    // `onsubmit` and the textarea's `onkeydown` without a "closure moved twice" conflict.
    let submit = use_callback(move |_: ()| {
        let text = input.read().trim().to_string();
        if text.is_empty() || sending() {
            return;
        }

        if text.starts_with('/') {
            let session_snapshot = session.read().clone();
            let sending_snapshot = sending();
            let message_count = messages.read().len();
            let base = base_for_slash.clone();
            let text_owned = text.clone();
            let theme_snapshot = theme_name.read().clone();

            #[cfg(target_arch = "wasm32")]
            wasm_bindgen_futures::spawn_local(async move {
                let (cmd_msgs, new_session, results) = handle_slash_command(
                    &text_owned,
                    &base,
                    session_snapshot,
                    sending_snapshot,
                    message_count,
                    &theme_snapshot,
                )
                .await;
                for msg in cmd_msgs {
                    messages.write().push(msg);
                }
                if let Some(id) = new_session {
                    session.set(Some(id));
                }
                for result in &results {
                    match result {
                        CommandResult::NewConversation { .. } => {
                            messages.set(Vec::new());
                            session.set(None);
                            active_conv_id.set(None);
                        }
                        CommandResult::ChatCleared => {
                            messages.set(Vec::new());
                        }
                        CommandResult::SessionSwitched { id } => {
                            session.set(Some(id.clone()));
                            active_conv_id.set(Some(id.clone()));
                        }
                        CommandResult::SessionClosed { .. } => {
                            session.set(None);
                        }
                        // The command announces the browser; opening it is the surface's job. This
                        // arm is what was missing: the result fell into `_` and `/model` printed
                        // "Opening model browser" while nothing opened.
                        CommandResult::OpenModelBrowser => {
                            model_browser_open.set(true);
                        }
                        CommandResult::OpenThemeBrowser => {
                            theme_browser_open.set(true);
                        }
                        // The command layer validated the name against the shared registry; all that
                        // is left is to render it and remember it.
                        CommandResult::ThemeChanged { name } => {
                            theme_name.set(name.clone());
                            crate::theme::save_theme_name(name);
                        }
                        _ => {}
                    }
                }
            });
            #[cfg(not(target_arch = "wasm32"))]
            {
                let _ = (
                    base,
                    text_owned,
                    session_snapshot,
                    sending_snapshot,
                    message_count,
                    theme_snapshot,
                );
            }

            input.set(String::new());
            resize_input_to_content();
            return;
        }

        messages.write().push(ChatMsg::new_user(text.clone()));
        input.set(String::new());
        resize_input_to_content();
        sending.set(true);

        if session.read().is_none() {
            should_set_title.set(true);
        }

        #[cfg(target_arch = "wasm32")]
        open_stream(
            &base_for_submit,
            &text,
            StreamTargets {
                messages,
                sending,
                session,
                active_conv_id,
                should_set_title,
                ghost_session,
            },
            &base_for_title,
            incognito(),
        );
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = &base_for_submit;
            let _ = &session;
            let _ = &active_conv_id;
            let _ = &should_set_title;
            let _ = &base_for_title;
            sending.set(false);
        }
    });

    let conv_id = active_conv_id.read().clone();
    let chat_cls = if incognito() {
        "chat incognito"
    } else {
        "chat"
    };

    rsx! {
        div {
            class: "{chat_cls}",

            if incognito() {
                // Stated where the conversation is, not only on the button in the header — the
                // wrong thing to be unsure about mid-chat is whether it is being recorded. The
                // second sentence is the honest limit of the promise.
                div {
                    class: "incognito-banner",
                    span { class: "incognito-glyph", "\u{1F576}" }
                    span {
                        b { "Incognito." }
                        " This chat is never written to disk and is discarded when you leave it. Actions the agent takes — notes, memories, files — still happen."
                    }
                }
            }

            div {
                class: "messages",
                if messages.read().is_empty() && conv_id.is_none() && !sending() && ghost_session.read().is_none() {
                    div {
                        class: "empty-state",
                        p { "Start a conversation with Liberado." }
                        p { "It has access to your tools." }
                    }
                }
                for (i, msg) in messages.read().iter().enumerate() {
                    MessageRow { key: "{i}", msg: msg.clone() }
                }
                if sending() {
                    div { class: "bubble-row assistant",
                        div { class: "bubble-thinking", "\u{2026}" }
                    }
                }
            }

            if theme_browser_open() {
                Picker {
                    title: "Switch theme",
                    current: Some(theme_name()),
                    items: crate::theme::theme_names(),
                    status: None,
                    error: None,
                    open: theme_browser_open,
                    // A theme applies instantly and locally, so unlike the model picker there is no
                    // round trip to wait on: close as soon as it is chosen.
                    on_pick: move |name: String| {
                        theme_name.set(name.clone());
                        crate::theme::save_theme_name(&name);
                        theme_browser_open.set(false);
                        messages
                            .write()
                            .push(ChatMsg {
                                role: "system",
                                content: format!("Theme: {name}"),
                                thinking_steps: Vec::new(),
                            });
                    },
                }
            }

            if model_browser_open() {
                ModelBrowser {
                    api_base: base_for_models.clone(),
                    open: model_browser_open,
                    on_switched: move |model: String| {
                        messages
                            .write()
                            .push(ChatMsg {
                                role: "system",
                                content: format!("Model switched to {model} (hot-swap, no restart)."),
                                thinking_steps: Vec::new(),
                            });
                    },
                }
            }

            form {
                class: "input-bar",
                onsubmit: move |evt| {
                    evt.prevent_default();
                    submit.call(());
                },
                textarea {
                    id: "chat-input",
                    class: "input",
                    placeholder: "Message Liberado\u{2026}",
                    value: "{input}",
                    rows: 1,
                    autofocus: true,
                    oninput: move |e| {
                        input.set(e.value());
                        resize_input_to_content();
                    },
                    onkeydown: move |e: Event<KeyboardData>| {
                        if e.key() == Key::Enter && !e.modifiers().contains(Modifiers::SHIFT) {
                            e.prevent_default();
                            submit.call(());
                        }
                    },
                }
                if sending() {
                    button {
                        class: "stop-btn",
                        r#type: "button",
                        onclick: move |_| stop_stream(),
                        title: "Stop generating",
                        "\u{23F9}"
                    }
                }
                button {
                    class: "send-btn",
                    r#type: "submit",
                    disabled: sending(),
                    "Send"
                }
            }
        }
    }
}

// ── Message row — renders a message + optional thinking steps ───────────────

#[component]
fn MessageRow(msg: ChatMsg) -> Element {
    let has_steps = !msg.thinking_steps.is_empty();

    rsx! {
        div {
            class: "bubble-row {msg.role}",
            div {
                class: "bubble-wrap",
                if has_steps {
                    ThinkingGroup { steps: msg.thinking_steps.clone() }
                }
                if !msg.content.is_empty() || !has_steps {
                    match msg.role {
                        "assistant" | "user" => rsx! {
                            div { class: "bubble {msg.role}",
                                MarkdownText { content: msg.content.clone() }
                            }
                        },
                        "tool" => rsx! { ToolBlock { content: msg.content.clone() } },
                        _ => rsx! {
                            div { class: "bubble {msg.role}",
                                "{msg.content}"
                            }
                        },
                    }
                }
            }
        }
    }
}

// ── Collapsible tool result ─────────────────────────────────────────────────

/// A `tool` message — the full text a dispatched tool returned, as replayed from conversation
/// history. This is the block that used to be permanently open: the live turn shows a
/// [`ThinkingGroup`], but history has no thinking steps (they exist only on the SSE stream), so a
/// reloaded conversation rendered the whole result as a plain bubble with no way to fold it away.
/// Several hundred characters of session ids and journal paths then sat between the question and
/// the answer.
///
/// Collapsed by default, with the outcome line kept in the header, and it stays wherever the user
/// puts it.
#[component]
fn ToolBlock(content: String) -> Element {
    let mut expanded = use_signal(|| false);

    // The daemon's first line is already a summary ("RESULT (Succeeded):"). Reuse it as the header
    // rather than inventing one, falling back only if it is empty so the header is never blank.
    let label = content
        .lines()
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .unwrap_or("Tool result")
        .trim_end_matches(':')
        .to_string();
    let arrow = if expanded() { "\u{25BC}" } else { "\u{25B8}" };

    rsx! {
        div {
            class: "thinking-group",
            button {
                class: "thinking-header",
                // Explicit: a bare <button> defaults to type=submit, which would post the chat
                // form the moment this block is ever rendered inside one.
                r#type: "button",
                onclick: move |_| {
                    let now = expanded();
                    expanded.set(!now);
                },
                span { class: "thinking-arrow", "{arrow}" }
                span { class: "thinking-label", "\u{1F527} {label}" }
            }
            if expanded() {
                div {
                    class: "thinking-body",
                    div { class: "tool-result-body", "{content}" }
                }
            }
        }
    }
}

// ── Collapsible thinking-steps group ────────────────────────────────────────

#[component]
fn ThinkingGroup(steps: Vec<ThinkingStep>) -> Element {
    // Starts collapsed and then belongs entirely to the user — nothing derived from `steps` ever
    // moves it again. It used to open itself whenever a step was pending, which meant the run
    // decided the disclosure state instead of the reader: it sprang open mid-turn, and wherever it
    // happened to be when the last step resolved was where it stuck. Progress is already in the
    // header label, which is the part that stays visible while collapsed.
    let mut expanded = use_signal(|| false);

    let has_pending = steps.iter().any(|s| s.ok.is_none());
    let toggle = move |_| {
        let now = expanded();
        expanded.set(!now);
    };

    let count = steps.len();
    let summary: Vec<String> = steps.iter().map(|s| s.tool_name.clone()).collect();
    let summary_text = summary.join(", ");

    let header_arrow = if expanded() { "\u{25BC}" } else { "\u{25B8}" };
    let header_label = if has_pending {
        format!(
            "Thinking ({count} step{plural}): {summary_text} \u{2026}",
            plural = if count == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "Thinking ({count} step{plural}): {summary_text}",
            plural = if count == 1 { "" } else { "s" }
        )
    };

    rsx! {
        div {
            class: "thinking-group",
            button {
                class: "thinking-header",
                r#type: "button",
                onclick: toggle,
                span { class: "thinking-arrow", "{header_arrow}" }
                span { class: "thinking-label", "{header_label}" }
            }
            if expanded() {
                div {
                    class: "thinking-body",
                    for step in steps.iter() {
                        ThinkingStepRow { step: step.clone() }
                    }
                }
            }
        }
    }
}

#[component]
fn ThinkingStepRow(step: ThinkingStep) -> Element {
    let status_cls = match step.ok {
        None => "thinking-step pending",
        Some(true) => "thinking-step ok",
        Some(false) => "thinking-step err",
    };

    let args_display =
        if step.tool_args.is_empty() || step.tool_args == "{}" || step.tool_args == "null" {
            String::new()
        } else {
            format!("({})", step.tool_args)
        };

    let mark = match step.ok {
        None => "\u{23F3}",
        Some(true) => "\u{2713}",
        Some(false) => "\u{2717}",
    };

    let name_text = format!("\u{1F527} {}{}", step.tool_name, args_display);

    rsx! {
        div {
            class: "{status_cls}",
            span { class: "thinking-step-name", "{name_text}" }
            span { class: "thinking-step-mark", "{mark}" }
            if !step.preview.is_empty() {
                span { class: "thinking-step-preview", "{step.preview}" }
            }
        }
    }
}

// ── Input auto-grow (browser-only) ──────────────────────────────────────────

/// Grow the `#chat-input` textarea to fit its content. Resets to `auto` first so deleting text
/// shrinks it back down (reading `scroll_height` without that reset would just echo the current,
/// already-grown height). The CSS `max-height` on `.input` silently caps the visible box and
/// turns on the scrollbar once content exceeds it — this only needs to set the natural height.
#[cfg(target_arch = "wasm32")]
fn resize_input_to_content() {
    use wasm_bindgen::JsCast;

    let Some(el) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("chat-input"))
        .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return;
    };
    let style = el.style();
    let _ = style.set_property("height", "auto");
    let _ = style.set_property("height", &format!("{}px", el.scroll_height()));
}

#[cfg(not(target_arch = "wasm32"))]
fn resize_input_to_content() {}

// ── SSE streaming (browser-only) ────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
thread_local! {
    static CURRENT_SOURCE: std::cell::RefCell<Option<std::rc::Rc<web_sys::EventSource>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(target_arch = "wasm32")]
fn close_current_stream() {
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
#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy)]
struct StreamTargets {
    messages: Signal<Vec<ChatMsg>>,
    sending: Signal<bool>,
    session: Signal<Option<String>>,
    /// Left untouched by an incognito turn — that session is not in the sidebar to be selected.
    active_conv_id: Signal<Option<String>>,
    should_set_title: Signal<bool>,
    /// Where an incognito turn records the session it opened, for the teardown paths.
    ghost_session: Signal<Option<String>>,
}

#[cfg(target_arch = "wasm32")]
fn open_stream(
    api_base: &str,
    message: &str,
    targets: StreamTargets,
    api_base_for_title: &str,
    incognito: bool,
) {
    let StreamTargets {
        mut messages,
        mut sending,
        mut session,
        mut active_conv_id,
        mut should_set_title,
        mut ghost_session,
    } = targets;
    use std::rc::Rc;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;
    use web_sys::{EventSource, MessageEvent};

    let encoded = urlencoding::encode(message);
    // `incognito` describes how to *open* a session, so it rides only on the request that has no
    // `session` — sending it alongside an existing id would suggest an already-durable conversation
    // could be retroactively made private, which is not a thing the daemon can do.
    let url = match session.read().as_ref() {
        Some(id) => format!("{api_base}/api/chat/stream?message={encoded}&session={id}"),
        None if incognito => format!("{api_base}/api/chat/stream?message={encoded}&incognito=1"),
        None => format!("{api_base}/api/chat/stream?message={encoded}"),
    };
    let ghost_base = api_base.to_string();
    let source = match EventSource::new(&url) {
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
                if incognito {
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
                    Some(last) if last.role == "assistant" => last.content.push_str(&data),
                    _ => m.push(ChatMsg::new_assistant(data)),
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
            if let Some(data) = e.data().as_string() {
                if let Ok(chat_client_contract::SessionEvent {
                    kind: chat_client_contract::SessionEventKind::ToolStarted { name, args_preview },
                    ..
                }) = chat_client_contract::SessionEvent::from_sse_data("tool_started", &data)
                {
                    let clean_args = if args_preview == "{}" || args_preview == "null" {
                        String::new()
                    } else {
                        args_preview
                    };
                    messages.with_mut(|m| match m.last_mut() {
                        Some(last) if last.role == "assistant" => {
                            last.thinking_steps.push(ThinkingStep {
                                tool_name: name,
                                tool_args: clean_args,
                                ok: None,
                                preview: String::new(),
                            });
                        }
                        _ => {
                            let mut msg = ChatMsg::new_assistant(String::new());
                            msg.thinking_steps.push(ThinkingStep {
                                tool_name: name,
                                tool_args: clean_args,
                                ok: None,
                                preview: String::new(),
                            });
                            m.push(msg);
                        }
                    });
                }
            }
        });
        let _ = source
            .add_event_listener_with_callback("tool_started", on_tool.as_ref().unchecked_ref());
        on_tool.forget();
    }

    // tool_finished -> resolve the most recent pending ThinkingStep with matching name.
    {
        let on_result = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            if let Some(data) = e.data().as_string() {
                if let Ok(chat_client_contract::SessionEvent {
                    kind:
                        chat_client_contract::SessionEventKind::ToolFinished {
                            name,
                            ok,
                            result_preview: preview,
                        },
                    ..
                }) = chat_client_contract::SessionEvent::from_sse_data("tool_finished", &data)
                {
                    messages.with_mut(|m| {
                        // Find the last assistant message that has a pending step matching `name`.
                        // `find_map` hands back a `&mut ThinkingStep` borrowed from `m` itself —
                        // no raw pointer needed; NLL is fine with using it right after.
                        let found = m
                            .iter_mut()
                            .rev()
                            .filter(|msg| msg.role == "assistant")
                            .find_map(|msg| {
                                msg.thinking_steps
                                    .iter_mut()
                                    .rev()
                                    .find(|s| s.ok.is_none() && s.tool_name == name)
                            });
                        if let Some(step) = found {
                            step.ok = Some(ok);
                            step.preview = preview;
                        }
                    });
                }
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
            source_done.close();
            sending.set(false);
            CURRENT_SOURCE.with(|cell| *cell.borrow_mut() = None);

            // Skipped for incognito: a title exists to label a row in the sidebar, and an incognito
            // session has no row. Naming it would only mean sending the first thing the user typed
            // back over the wire for a field nobody will ever read.
            if should_set_title() && !incognito {
                should_set_title.set(false);
                let title_opt = messages.read().iter().find(|m| m.role == "user").map(|m| {
                    let t = m.content.trim();
                    if t.len() > 60 {
                        // Byte-length slicing panics if 57 lands mid-codepoint (any curly
                        // quote, em-dash, etc.) — walk back to the nearest char boundary.
                        let mut cut = 57;
                        while cut > 0 && !t.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        format!("{}…", &t[..cut])
                    } else {
                        t.to_string()
                    }
                });
                let conv_id_opt = session.read().clone();
                if let (Some(title), Some(conv_id)) = (title_opt, conv_id_opt) {
                    let base = title_base.clone();
                    web_sys::console::log_1(
                        &format!("[title] setting title for {}: {}", conv_id, title).into(),
                    );
                    wasm_bindgen_futures::spawn_local(async move {
                        let url = format!("{base}/api/conversations/{conv_id}");
                        let body = serde_json::json!({ "title": title });
                        let client = reqwest::Client::new();
                        match client.patch(&url).json(&body).send().await {
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

    // Native EventSource error (connection dropped).
    {
        let source_err = source.clone();
        let on_err = Closure::<dyn FnMut(MessageEvent)>::new(move |_e: MessageEvent| {
            source_err.close();
            sending.set(false);
            CURRENT_SOURCE.with(|cell| *cell.borrow_mut() = None);
        });
        source.set_onerror(Some(on_err.as_ref().unchecked_ref()));
        on_err.forget();
    }
}
