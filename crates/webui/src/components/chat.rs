use dioxus::prelude::*;

use chat_client_contract::ChatMessage;

use crate::components::markdown::MarkdownText;
use crate::components::slash_commands::handle_slash_command;
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
        ChatMsg { role: "user", content, thinking_steps: Vec::new() }
    }

    fn new_assistant(content: String) -> Self {
        ChatMsg { role: "assistant", content, thinking_steps: Vec::new() }
    }

    fn new_error(content: String) -> Self {
        ChatMsg { role: "error", content, thinking_steps: Vec::new() }
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

async fn fetch_conversation(
    api_base: &str,
    conv_id: &str,
) -> Result<Vec<ChatMsg>, String> {
    let url = format!("{api_base}/api/conversations/{conv_id}");
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    let history: chat_client_contract::ConversationHistoryResponse = resp
        .json()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    Ok(history.messages.iter().map(ChatMsg::from_wire).collect())
}

// ── Chat component ──────────────────────────────────────────────────────────

#[component]
pub fn Chat(api_base: String, mut active_conv_id: Signal<Option<String>>) -> Element {
    let mut messages = use_signal(Vec::new);
    let mut input = use_signal(String::new);
    let mut sending = use_signal(|| false);
    let mut session = use_signal(|| None::<String>);
    let mut should_set_title = use_signal(|| false);

    let base_for_effect = api_base.clone();
    let base_for_submit = api_base.clone();
    let base_for_title = api_base.clone();
    let base_for_slash = api_base.clone();

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
            } else {
                messages.set(Vec::new());
                session.set(None);
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = &id;
            let _ = &base;
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

            #[cfg(target_arch = "wasm32")]
            wasm_bindgen_futures::spawn_local(async move {
                let (cmd_msgs, new_session, results) = handle_slash_command(
                    &text_owned,
                    &base,
                    session_snapshot,
                    sending_snapshot,
                    message_count,
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
                        _ => {}
                    }
                }
            });
            #[cfg(not(target_arch = "wasm32"))]
            {
                let _ = (base, text_owned, session_snapshot, sending_snapshot, message_count);
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
        open_stream(&base_for_submit, &text, messages, sending, session, active_conv_id, should_set_title, &base_for_title);
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

    rsx! {
        div {
            class: "chat",

            div {
                class: "messages",
                if messages.read().is_empty() && conv_id.is_none() && !sending() {
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

// ── Collapsible thinking-steps group ────────────────────────────────────────

#[component]
fn ThinkingGroup(steps: Vec<ThinkingStep>) -> Element {
    let mut expanded = use_signal(|| steps.iter().any(|s| s.ok.is_none()));

    let has_pending = steps.iter().any(|s| s.ok.is_none());
    let toggle = move |_| expanded.set(!expanded());

    let count = steps.len();
    let summary: Vec<String> = steps.iter().map(|s| s.tool_name.clone()).collect();
    let summary_text = summary.join(", ");

    let header_arrow = if expanded() { "\u{25BC}" } else { "\u{25B8}" };
    let header_label = if has_pending {
        format!("Thinking ({count} step{plural}): {summary_text} \u{2026}",
            plural = if count == 1 { "" } else { "s" })
    } else {
        format!("Thinking ({count} step{plural}): {summary_text}",
            plural = if count == 1 { "" } else { "s" })
    };

    rsx! {
        div {
            class: "thinking-group",
            button {
                class: "thinking-header",
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

    let args_display = if step.tool_args.is_empty()
        || step.tool_args == "{}"
        || step.tool_args == "null"
    {
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

#[cfg(target_arch = "wasm32")]
fn open_stream(
    api_base: &str,
    message: &str,
    mut messages: Signal<Vec<ChatMsg>>,
    mut sending: Signal<bool>,
    mut session: Signal<Option<String>>,
    mut active_conv_id: Signal<Option<String>>,
    mut should_set_title: Signal<bool>,
    api_base_for_title: &str,
) {
    use std::rc::Rc;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;
    use web_sys::{EventSource, MessageEvent};

    let encoded = urlencoding::encode(message);
    let url = match session.read().as_ref() {
        Some(id) => format!("{api_base}/api/chat/stream?message={encoded}&session={id}"),
        None => format!("{api_base}/api/chat/stream?message={encoded}"),
    };
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

    // session -> record it and update active_conv_id so the sidebar highlights it.
    {
        let on_session = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            if let Some(data) = e.data().as_string() {
                if !data.is_empty() {
                    session.set(Some(data.clone()));
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
        let _ =
            source.add_event_listener_with_callback("token", on_token.as_ref().unchecked_ref());
        on_token.forget();
    }

    // tool -> append a pending ThinkingStep to the last assistant message (creating one if needed).
    {
        let on_tool = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            if let Some(data) = e.data().as_string() {
                if let Ok(chat_client_contract::ChatEvent::Tool { name, args }) =
                    chat_client_contract::ChatEvent::from_sse_data("tool", &data)
                {
                    let args_str = args.to_string();
                    let clean_args = if args_str == "{}" || args_str == "null" {
                        String::new()
                    } else {
                        args_str
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
        let _ =
            source.add_event_listener_with_callback("tool", on_tool.as_ref().unchecked_ref());
        on_tool.forget();
    }

    // tool_result -> resolve the most recent pending ThinkingStep with matching name.
    {
        let on_result = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            if let Some(data) = e.data().as_string() {
                if let Ok(chat_client_contract::ChatEvent::ToolResult { name, ok, preview }) =
                    chat_client_contract::ChatEvent::from_sse_data("tool_result", &data)
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
            .add_event_listener_with_callback("tool_result", on_result.as_ref().unchecked_ref());
        on_result.forget();
    }

    // done -> close + stop + optionally set title.
    {
        let source_done = source.clone();
        let title_base = api_base_for_title.to_string();
        let on_done = Closure::<dyn FnMut(MessageEvent)>::new(move |_e: MessageEvent| {
            source_done.close();
            sending.set(false);
            CURRENT_SOURCE.with(|cell| *cell.borrow_mut() = None);

            if should_set_title() {
                should_set_title.set(false);
                let title_opt = messages
                    .read()
                    .iter()
                    .find(|m| m.role == "user")
                    .map(|m| {
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
                    web_sys::console::log_1(&format!("[title] setting title for {}: {}", conv_id, title).into());
                    wasm_bindgen_futures::spawn_local(async move {
                        let url = format!("{base}/api/conversations/{conv_id}");
                        let body = serde_json::json!({ "title": title });
                        let client = reqwest::Client::new();
                        match client.patch(&url).json(&body).send().await {
                            Ok(resp) => {
                                if !resp.status().is_success() {
                                    web_sys::console::log_1(&format!("[title] PATCH failed: {} - {}", resp.status(), resp.text().await.unwrap_or_default()).into());
                                }
                            }
                            Err(e) => {
                                web_sys::console::log_1(&format!("[title] PATCH error: {}", e).into());
                            }
                        }
                    });
                } else {
                    web_sys::console::log_1(&"[title] should_set_title was true but no user message or session".into());
                }
            }
        });
        let _ =
            source.add_event_listener_with_callback("done", on_done.as_ref().unchecked_ref());
        on_done.forget();
    }

    // failed -> replace in-flight message with error, close + stop.
    {
        let source_fail = source.clone();
        let on_fail = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            let msg = e
                .data()
                .as_string()
                .unwrap_or_else(|| "stream error".into());
            messages.with_mut(|m| {
                m.push(ChatMsg::new_error(msg))
            });
            source_fail.close();
            sending.set(false);
            CURRENT_SOURCE.with(|cell| *cell.borrow_mut() = None);
        });
        let _ =
            source.add_event_listener_with_callback("failed", on_fail.as_ref().unchecked_ref());
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
