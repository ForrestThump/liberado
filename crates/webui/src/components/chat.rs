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
            class: "flex flex-col h-[calc(100vh-9rem)] rounded-xl border border-gray-800 bg-gray-900/50 overflow-hidden",

            // --- message list ---
            div {
                class: "flex-1 overflow-y-auto p-4 space-y-4",
                if messages.read().is_empty() {
                    div {
                        class: "h-full flex flex-col items-center justify-center text-gray-600",
                        p { class: "text-sm", "Start a conversation with Liberado." }
                        p { class: "text-xs mt-1 text-gray-700", "It has access to your tools." }
                    }
                }
                for (i, msg) in messages.read().iter().enumerate() {
                    Bubble { key: "{i}", role: msg.role, content: msg.content.clone() }
                }
                if sending() {
                    div { class: "flex justify-start",
                        div { class: "bg-gray-800 rounded-2xl px-4 py-2 text-sm text-gray-500 italic",
                            "…"
                        }
                    }
                }
            }

            // --- input ---
            form {
                class: "border-t border-gray-800 p-3 flex gap-2 bg-gray-900/80",
                onsubmit: move |evt| {
                    evt.prevent_default();
                    submit();
                },
                input {
                    class: "flex-1 bg-gray-800 rounded-lg px-3 py-2 text-sm text-gray-100 placeholder-gray-600 outline-none focus:ring-1 focus:ring-indigo-500",
                    placeholder: "Message Liberado…",
                    value: "{input}",
                    autofocus: true,
                    oninput: move |e| input.set(e.value()),
                }
                button {
                    class: "bg-indigo-600 hover:bg-indigo-500 disabled:opacity-40 rounded-lg px-4 py-2 text-sm font-medium transition-colors",
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
                let v: serde_json::Value = serde_json::from_str(&data).unwrap_or_default();
                let name = v["name"].as_str().unwrap_or("tool");
                let args = v["args"].as_str().unwrap_or("");
                let label = if args.is_empty() || args == "{}" {
                    format!("🔧 {name}…")
                } else {
                    format!("🔧 {name}({args})…")
                };
                messages.with_mut(|m| {
                    m.push(ChatMsg {
                        role: "tool",
                        content: label,
                    })
                });
            }
        });
        let _ = source.add_event_listener_with_callback("tool", on_tool.as_ref().unchecked_ref());
        on_tool.forget();
    }

    // tool_result → resolve the most recent tool chip with its outcome: JSON `{name, ok, preview}`.
    {
        let on_result = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            if let Some(data) = e.data().as_string() {
                let v: serde_json::Value = serde_json::from_str(&data).unwrap_or_default();
                let name = v["name"].as_str().unwrap_or("tool");
                let ok = v["ok"].as_bool().unwrap_or(false);
                let prev = v["preview"].as_str().unwrap_or("");
                let mark = if ok { "✓" } else { "✗" };
                let label = if prev.is_empty() {
                    format!("🔧 {name} {mark}")
                } else {
                    format!("🔧 {name} {mark} {prev}")
                };
                messages.with_mut(|m| match m.iter_mut().rev().find(|x| x.role == "tool") {
                    Some(chip) => chip.content = label,
                    None => m.push(ChatMsg {
                        role: "tool",
                        content: label,
                    }),
                });
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
    // Tool chips render compact and monospaced — a status line, not a speech bubble.
    if role == "tool" {
        return rsx! {
            div {
                class: "flex justify-start",
                div {
                    class: "max-w-[80%] rounded-lg px-3 py-1 text-xs font-mono text-gray-400 bg-gray-800/60 border border-gray-700/60 whitespace-pre-wrap",
                    "{content}"
                }
            }
        };
    }

    let (align, bubble) = match role {
        "user" => ("justify-end", "bg-indigo-600 text-white"),
        "error" => (
            "justify-start",
            "bg-red-950/60 text-red-300 border border-red-800",
        ),
        _ => ("justify-start", "bg-gray-800 text-gray-100"),
    };
    rsx! {
        div {
            class: "flex {align}",
            div {
                class: "max-w-[80%] rounded-2xl px-4 py-2 text-sm leading-relaxed whitespace-pre-wrap {bubble}",
                "{content}"
            }
        }
    }
}
