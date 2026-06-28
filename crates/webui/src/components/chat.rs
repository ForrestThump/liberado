use dioxus::prelude::*;

/// One rendered chat message.
#[derive(Clone, PartialEq)]
struct ChatMsg {
    role: &'static str, // "user" | "assistant" | "tool" | "error"
    content: String,
}

#[component]
pub fn Chat(api_base: String) -> Element {
    let mut messages = use_signal(Vec::<ChatMsg>::new);
    let mut input = use_signal(String::new);
    let mut sending = use_signal(|| false);
    // The conversation this chat is bound to. `None` until the first turn, when the server creates a
    // conversation and announces its id via the `session` SSE event; we then send it back on every
    // subsequent turn so they continue the same conversation rather than starting fresh.
    let session = use_signal(|| None::<String>);

    let mut submit = move || {
        let text = input.read().trim().to_string();
        if text.is_empty() || sending() {
            return;
        }
        messages.write().push(ChatMsg {
            role: "user",
            content: text.clone(),
        });
        // The assistant bubble is created lazily on the first `token`, so any `tool` chips for this
        // turn land *before* the answer (tool events precede the prose that follows them).
        input.set(String::new());
        sending.set(true);

        #[cfg(target_arch = "wasm32")]
        open_stream(&api_base, &text, messages, sending, session);
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = &api_base;
            let _ = &session;
            sending.set(false);
        }
    };

    rsx! {
        div {
            class: "chat",

            // --- message list ---
            div {
                class: "messages",
                if messages.read().is_empty() {
                    div {
                        class: "empty-state",
                        p { "Start a conversation with Liberado." }
                        p { "It has access to your tools." }
                    }
                }
                for (i, msg) in messages.read().iter().enumerate() {
                    Bubble { key: "{i}", role: msg.role, content: msg.content.clone() }
                }
                if sending() {
                    div { class: "bubble-row assistant",
                        div { class: "bubble-thinking", "…" }
                    }
                }
            }

            // --- input ---
            form {
                class: "input-bar",
                onsubmit: move |evt| {
                    evt.prevent_default();
                    submit();
                },
                input {
                    class: "input",
                    placeholder: "Message Liberado…",
                    value: "{input}",
                    autofocus: true,
                    oninput: move |e| input.set(e.value()),
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

/// Open an SSE stream for one turn and pipe its events into the signals. Browser-only: uses the
/// native `EventSource` (the `GET` variant of `/api/chat/stream`). The closures are `forget`-leaked
/// so they outlive this call; the stream closes itself on `done`/`failed`.
#[cfg(target_arch = "wasm32")]
fn open_stream(
    api_base: &str,
    message: &str,
    mut messages: Signal<Vec<ChatMsg>>,
    mut sending: Signal<bool>,
    mut session: Signal<Option<String>>,
) {
    use std::rc::Rc;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;
    use web_sys::{EventSource, MessageEvent};

    let encoded = urlencoding::encode(message);
    // Continue the existing conversation when we have its id; the first turn omits it and the server
    // creates one, returning the id on the `session` event below.
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

    // session → the server's conversation id for this stream; record it so subsequent turns send it
    // back as `?session=…` and continue this conversation instead of creating a new one.
    {
        let on_session = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            if let Some(data) = e.data().as_string() {
                if !data.is_empty() {
                    session.set(Some(data));
                }
            }
        });
        let _ =
            source.add_event_listener_with_callback("session", on_session.as_ref().unchecked_ref());
        on_session.forget();
    }

    // token → append the delta, creating the assistant bubble on first sight (so tool chips for
    // this turn precede it).
    {
        let on_token = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            if let Some(data) = e.data().as_string() {
                messages.with_mut(|m| match m.last_mut() {
                    Some(last) if last.role == "assistant" => last.content.push_str(&data),
                    _ => m.push(ChatMsg {
                        role: "assistant",
                        content: data,
                    }),
                });
            }
        });
        let _ = source.add_event_listener_with_callback("token", on_token.as_ref().unchecked_ref());
        on_token.forget();
    }

    // tool → a chip for the call starting: `data` is JSON `{name, args}`.
    {
        let on_tool = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            if let Some(data) = e.data().as_string() {
                if let Ok(chat_client_contract::ChatEvent::Tool { name, args }) =
                    chat_client_contract::ChatEvent::from_sse_data("tool", &data)
                {
                    let args_str = args.to_string();
                    let label = if args_str.is_empty() || args_str == "{}" || args_str == "null" {
                        format!("🔧 {name}…")
                    } else {
                        format!("🔧 {name}({args_str})…")
                    };
                    messages.with_mut(|m| {
                        m.push(ChatMsg {
                            role: "tool",
                            content: label,
                        })
                    });
                }
            }
        });
        let _ = source.add_event_listener_with_callback("tool", on_tool.as_ref().unchecked_ref());
        on_tool.forget();
    }

    // tool_result → resolve the most recent tool chip with its outcome: JSON `{name, ok, preview}`.
    {
        let on_result = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            if let Some(data) = e.data().as_string() {
                if let Ok(chat_client_contract::ChatEvent::ToolResult { name, ok, preview }) =
                    chat_client_contract::ChatEvent::from_sse_data("tool_result", &data)
                {
                    let mark = if ok { "✓" } else { "✗" };
                    let label = if preview.is_empty() {
                        format!("🔧 {name} {mark}")
                    } else {
                        format!("🔧 {name} {mark} {preview}")
                    };
                    messages.with_mut(|m| match m.iter_mut().rev().find(|x| x.role == "tool") {
                        Some(chip) => chip.content = label,
                        None => m.push(ChatMsg {
                            role: "tool",
                            content: label,
                        }),
                    });
                }
            }
        });
        let _ = source
            .add_event_listener_with_callback("tool_result", on_result.as_ref().unchecked_ref());
        on_result.forget();
    }

    // done → close + stop.
    {
        let source_done = source.clone();
        let on_done = Closure::<dyn FnMut(MessageEvent)>::new(move |_e: MessageEvent| {
            source_done.close();
            sending.set(false);
        });
        let _ = source.add_event_listener_with_callback("done", on_done.as_ref().unchecked_ref());
        on_done.forget();
    }

    // failed → replace the in-flight message with the error, close + stop.
    {
        let source_fail = source.clone();
        let on_fail = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            let msg = e
                .data()
                .as_string()
                .unwrap_or_else(|| "stream error".into());
            messages.with_mut(|m| {
                m.push(ChatMsg {
                    role: "error",
                    content: msg,
                })
            });
            source_fail.close();
            sending.set(false);
        });
        let _ = source.add_event_listener_with_callback("failed", on_fail.as_ref().unchecked_ref());
        on_fail.forget();
    }

    // Native EventSource error (connection dropped). Close to stop auto-reconnect; don't overwrite
    // a message we may already have streamed.
    {
        let source_err = source.clone();
        let on_err = Closure::<dyn FnMut(MessageEvent)>::new(move |_e: MessageEvent| {
            source_err.close();
            sending.set(false);
        });
        source.set_onerror(Some(on_err.as_ref().unchecked_ref()));
        on_err.forget();
    }
}

#[component]
fn Bubble(role: &'static str, content: String) -> Element {
    rsx! {
        div {
            class: "bubble-row {role}",
            div {
                class: "bubble {role}",
                "{content}"
            }
        }
    }
}
