use dioxus::prelude::*;

use chat_client_contract::ChatMessage;

use crate::components::markdown::MarkdownText;
use crate::components::model_browser::ModelBrowser;
use crate::components::picker::Picker;
use crate::components::profile_browser::ProfileBrowser;
use crate::components::slash_palette::SlashPalette;

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

/// A loaded conversation: its visible messages, and the profile it runs under.
struct LoadedConversation {
    messages: Vec<ChatMsg>,
    profile: Option<String>,
    /// A turn is in flight for this conversation right now — so the transcript is missing a reply
    /// that is still coming, and this client should attach rather than assume it was lost.
    turn_running: bool,
    /// The last turn ended with no reply — usually the daemon restarting mid-inference. Rendered as
    /// a note rather than left as silence, which reads as "the model said nothing".
    turn_unanswered: bool,
}

async fn fetch_conversation(api_base: &str, conv_id: &str) -> Result<LoadedConversation, String> {
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
    Ok(LoadedConversation {
        messages: history
            .messages
            .iter()
            .filter(|m| m.role != "system")
            .map(ChatMsg::from_wire)
            .collect(),
        profile: history.profile,
        turn_running: history.turn_running,
        turn_unanswered: history.turn_unanswered,
    })
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
    /// Bumped by the sidebar's "New Chat". An explicit request, because it cannot be inferred from
    /// `active_conv_id` — an incognito chat never sets one.
    new_chat_nonce: Signal<u64>,
    /// The `/model` picker. Owned by `App` rather than here, along with every other dismissible
    /// layer, so that one place can order them for the Back gesture (see `back_nav.rs`). Chat still
    /// opens it — the command that asks for it is dispatched from `submit` below.
    model_browser_open: Signal<bool>,
    /// The `/theme` picker. Same ownership story as `model_browser_open`.
    theme_browser_open: Signal<bool>,
    /// Set true by Back (and by Esc) to hide the slash palette. Owned by `App` for the same reason
    /// the pickers are: it is a dismissible layer, and one place has to order them.
    palette_dismissed: Signal<bool>,
    /// Written here, read by `App`: whether the palette is actually on screen. `App` cannot derive it
    /// — openness depends on the input text, which lives in this component.
    palette_visible: Signal<bool>,
    /// The `/profile` picker. Same ownership story as the other pickers.
    profile_browser_open: Signal<bool>,
    /// Which session profile the open conversation runs under, for the picker's active badge and the
    /// header chip. Loaded from the conversation, not guessed.
    active_profile: Signal<Option<String>>,
) -> Element {
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut profile_browser_open = profile_browser_open;
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut active_profile = active_profile;
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut palette_dismissed = palette_dismissed;
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut model_browser_open = model_browser_open;
    // A model chosen before this conversation existed. Local to the chat rather than owned by `App`:
    // it is consumed by the very next message and means nothing outside that window. Cleared on send
    // — see `submit` — so a pick cannot leak onto a *later* chat the user opens instead.
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut pending_model = use_signal(|| None::<String>);
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut theme_browser_open = theme_browser_open;
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
    // Which palette row is selected — the TUI's `slash_palette_index` by another name, feeding the
    // same `liberado_commands` functions.
    let mut slash_index = use_signal(|| 0usize);

    let base_for_effect = api_base.clone();
    let base_for_submit = api_base.clone();
    let base_for_title = api_base.clone();
    let base_for_slash = api_base.clone();
    let base_for_models = api_base.clone();
    let base_for_profiles = api_base.clone();

    // `[webui] enter_key` from the daemon, read once. `true` until it answers — that is the
    // historical behaviour, so a slow or unreachable status endpoint degrades to what this composer
    // did before the setting existed rather than to the other mode.
    let base_for_enter = api_base.clone();
    let enter_cfg = use_resource(move || {
        let base = base_for_enter.clone();
        async move {
            reqwest::Client::new()
                .get(format!("{base}/api/status"))
                .send()
                .await
                .ok()?
                .json::<chat_client_contract::DaemonStatus>()
                .await
                .ok()
                .map(|s| s.enter_sends)
        }
    });
    let enter_sends = move || enter_cfg.read().as_ref().and_then(|v| *v).unwrap_or(true);

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
                    if let Ok(loaded) = fetch_conversation(&base, &conv_id).await {
                        let running = loaded.turn_running;
                        let unanswered = loaded.turn_unanswered;
                        let mut loaded_messages = loaded.messages;
                        // Derived at render time, never stored: this is a *reading* of the
                        // transcript, and writing it into the log would make a display decision
                        // permanent and re-answer it wrongly if the turn were later retried.
                        if unanswered {
                            loaded_messages.push(ChatMsg {
                                role: "system",
                                content: "That turn ended without a reply — the daemon most likely                                           restarted mid-answer. Nothing was saved; send again to retry."
                                    .to_string(),
                                thinking_steps: Vec::new(),
                            });
                        }
                        messages.set(loaded_messages);
                        // From the conversation, not remembered client-side: opening a chat in a
                        // second tab or after a restart must show the authority it actually runs
                        // under, not whatever this tab last set.
                        active_profile.set(loaded.profile);
                        // A turn is still going: rejoin it. This is the reload case — the answer is
                        // being written right now and the transcript cannot show it yet. Attaching
                        // rather than re-sending matters, because re-sending would start a second
                        // turn and charge for the same answer twice.
                        if running {
                            sending.set(true);
                            attach_stream(
                                &base,
                                &conv_id,
                                StreamTargets {
                                    messages,
                                    sending,
                                    session,
                                    active_conv_id,
                                    should_set_title,
                                    ghost_session,
                                },
                                &base,
                            );
                        }
                    }
                });
            } else if ghost_session.read().is_none() {
                messages.set(Vec::new());
                session.set(None);
                // A new chat starts on the default grant; leaving a stale chip up would claim
                // otherwise.
                active_profile.set(None);
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
    let mut prev_new_chat = use_signal(|| 0u64);
    use_effect(move || {
        let now = incognito();
        let was = prev_incognito();
        let nonce = new_chat_nonce();
        // "New Chat" while private starts a *new private chat* — the mode is not a per-conversation
        // setting you have to re-arm, same as a new tab in an incognito window.
        let asked_for_fresh = nonce != prev_new_chat();

        if now != was || asked_for_fresh {
            prev_incognito.set(now);
            prev_new_chat.set(nonce);
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

        // Still on, and a saved conversation got selected. Leave the private chat and disarm the
        // mode: what you are now looking at is written to disk, so the banner must not claim
        // otherwise. `messages`/`session` are left alone — the history effect above owns them now.
        //
        // Note there is **no `ghost_session.is_some()` condition** here. There used to be, and it
        // was half of a data-loss bug: arming the mode without sending anything leaves no ghost, so
        // this branch never fired, the banner sat over a saved conversation, and the mode stayed
        // armed while that conversation was the live one. Selecting a saved chat means leaving
        // incognito whether or not a private session was ever opened.
        if now && active_conv_id.read().is_some() {
            discard_ghost.call(());
            incognito.set(false);
            prev_incognito.set(false);
        }
    });

    // Stop now means "stop doing this", not "stop showing me this".
    //
    // Closing the EventSource used to be the cancel: the daemon saw its receiver go and dropped the
    // turn. Turns outlive their connection now, so closing alone would leave it running and only
    // hide it. The request is what stops it; closing the stream is still done so this client also
    // stops rendering.
    let mut stop_stream = move || {
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(id) = session() {
                let url = format!("{stop_base}/api/conversations/{id}/cancel");
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = reqwest::Client::new().post(&url).send().await;
                });
            }
            crate::components::chat::close_current_stream();
        }
        sending.set(false);
    };

    // `use_callback` (not a plain closure) so the same handle is `Copy` and can be moved into both
    // `onsubmit` and the textarea's `onkeydown` without a "closure moved twice" conflict.
    let submit = use_callback(move |_: ()| {
        let raw = input.read().clone();
        // Enter accepts the selected palette match without needing Tab first, so `/hel` + Enter runs
        // `/help`. Same rule and same function as the TUI's `send_message`.
        let text = match liberado_commands::accept_completion(&raw, slash_index()) {
            Some(completed) if !palette_dismissed() => completed.trim().to_string(),
            _ => raw.trim().to_string(),
        };
        if text.is_empty() || sending() {
            return;
        }
        slash_index.set(0);
        palette_dismissed.set(false);

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
                        CommandResult::OpenProfileBrowser => {
                            profile_browser_open.set(true);
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
            // Only meaningful when creating: an existing session's profile is already stored, and
            // the daemon ignores the field when a session id is present.
            active_profile(),
            // Unlike the profile, this is honoured with or without a session id — a model is a
            // property of a turn. Taken (not read) so it applies once and then lives on the log.
            pending_model.take(),
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

    // Shown while the input is a slash query that still matches something, and not while a turn is
    // streaming — the palette is for composing, and mid-stream the input is about to be replaced.
    let palette_open = use_memo(move || {
        !palette_dismissed()
            && !sending()
            && !crate::components::slash_palette::matches_for(&input()).is_empty()
    });
    // The remainder of the selected match, from the same function the TUI's ghost uses. `None` once
    // the typed text covers the whole command, so a complete `/help` shows no trailing artifact.
    let ghost_suffix = use_memo(move || {
        if !palette_open() {
            return None;
        }
        liberado_commands::ghost_suffix(&input(), slash_index())
    });
    // Report openness upward so `App` can put the palette in the Back-gesture layer stack. A mirror
    // rather than a prop: `App` has no access to the input text this is derived from.
    let mut palette_visible = palette_visible;
    use_effect(move || palette_visible.set(palette_open()));

    let chat_cls = if incognito() {
        "chat incognito"
    } else {
        "chat"
    };

    rsx! {
        div {
            class: "{chat_cls}",

            {
                // **Always rendered**, including with no profile set. It used to appear only once a
                // profile was chosen, which meant a new chat showed no control at all — you could
                // change a profile you already had, and had no way to acquire one except by knowing
                // `/profile` existed. A control that only exists after you have used it is not a
                // control.
                //
                // Shown in the conversation rather than in a menu: a profile changes what this chat
                // can do, and an authority you have to go looking for is one you will forget.
                let name = active_profile();
                let cls = if name.is_some() { "profile-chip set" } else { "profile-chip" };
                let label = name.unwrap_or_else(|| "default".to_string());
                rsx! {
                    button {
                        class: "{cls}",
                        r#type: "button",
                        title: "Session profile for this chat — click to change",
                        onclick: move |_| profile_browser_open.set(true),
                        span { class: "profile-chip-label", "profile" }
                        span { class: "profile-chip-name", "{label}" }
                        span { class: "profile-chip-caret", "\u{25BE}" }
                    }
                }
            }

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

            if profile_browser_open() {
                ProfileBrowser {
                    api_base: base_for_profiles.clone(),
                    session: session(),
                    current: active_profile(),
                    open: profile_browser_open,
                    on_switched: move |name: Option<String>| {
                        active_profile.set(name.clone());
                        messages
                            .write()
                            .push(ChatMsg {
                                role: "system",
                                content: match name {
                                    Some(n) => format!(
                                        "Session profile: {n} — applies from your next message."
                                    ),
                                    None => "Session profile cleared — back to the default grant,                                              from your next message."
                                        .to_string(),
                                },
                                thinking_steps: Vec::new(),
                            });
                    },
                }
            }

            if model_browser_open() {
                ModelBrowser {
                    api_base: base_for_models.clone(),
                    open: model_browser_open,
                    // Scope the pick to this chat once it has an id. Before that the browser hands
                    // the choice back and `pending_model` below carries it onto the request that
                    // creates the conversation.
                    conversation: session(),
                    on_switched: move |model: String| {
                        // Held for the next message when there is no conversation yet. Dropping it
                        // here is what sent the pick to the daemon-wide default instead, which
                        // retuned every other chat.
                        if session().is_none() {
                            pending_model.set(Some(model.clone()));
                        }
                        messages.write().push(ChatMsg {
                            role: "system",
                            content: format!(
                                "Model set to {model} for this conversation, from your next message. Other chats are unaffected."
                            ),
                            thinking_steps: Vec::new(),
                        });
                    },
                }
            }

            if palette_open() {
                SlashPalette {
                    input: input(),
                    selected: slash_index(),
                    // A tap is the phone's Tab. Fill the input rather than running it, so a
                    // command that takes an argument can still have one typed.
                    on_pick: move |idx: usize| {
                        if let Some(filled) = liberado_commands::complete_commands(&input(), idx) {
                            input.set(filled);
                            slash_index.set(idx);
                            resize_input_to_content();
                            focus_chat_input();
                        }
                    },
                }
            }

            form {
                class: "input-bar",
                onsubmit: move |evt| {
                    evt.prevent_default();
                    submit.call(());
                },
                // Wraps the textarea so the ghost mirror can sit exactly under it. `.input` keeps
                // its own metrics; this only supplies the positioning context.
                div {
                    class: "input-wrap",
                    // The dim remainder of the selected match, drawn *behind* a transparent-background
                    // textarea. The typed part is reproduced invisibly so the visible suffix starts
                    // precisely where the caret is — there is no way to measure that from Rust, so the
                    // browser measures it for us by laying out the same text in the same font.
                    if let Some(ghost) = ghost_suffix() {
                        div {
                            class: "input-ghost",
                            "aria-hidden": "true",
                            span { class: "input-ghost-typed", "{input}" }
                            span { class: "input-ghost-suffix", "{ghost}" }
                        }
                    }
                    textarea {
                        id: "chat-input",
                        class: "input",
                        placeholder: "Message Liberado\u{2026}",
                        value: "{input}",
                        rows: 1,
                        autofocus: true,
                        // The palette is a completion aid, not a listbox the caret moves into, so the
                        // textarea keeps focus and announces the relationship instead.
                        autocomplete: "off",
                        // Labels the virtual keyboard's action key to match what it will actually
                        // do. Cosmetic on a desktop; on a phone it is the difference between a key
                        // marked "send" that sends and one marked "return" that does not.
                        enterkeyhint: if enter_sends() { "send" } else { "enter" },
                        oninput: move |e| {
                            input.set(e.value());
                            // A changed query invalidates the old selection: `/s` selecting the third
                            // match and then typing `e` would otherwise leave the highlight on
                            // whatever now happens to be third.
                            slash_index.set(0);
                            palette_dismissed.set(false);
                            resize_input_to_content();
                        },
                        onkeydown: move |e: Event<KeyboardData>| {
                            let open = palette_open();
                            match e.key() {
                                // Tab fills progressively — the shared `complete_commands` decides
                                // how far, which is what keeps `/th` behaving as it does in the TUI.
                                Key::Tab if open => {
                                    e.prevent_default();
                                    if let Some(filled) =
                                        liberado_commands::complete_commands(&input(), slash_index())
                                    {
                                        input.set(filled);
                                        resize_input_to_content();
                                    }
                                }
                                Key::ArrowDown if open => {
                                    e.prevent_default();
                                    let n = crate::components::slash_palette::matches_for(&input()).len();
                                    if n > 0 {
                                        slash_index.set((slash_index() + 1).min(n - 1));
                                    }
                                }
                                Key::ArrowUp if open => {
                                    e.prevent_default();
                                    slash_index.set(slash_index().saturating_sub(1));
                                }
                                // Dismiss without clearing what was typed. Reopened by the next
                                // keystroke, since that is a new query.
                                Key::Escape if open => {
                                    e.prevent_default();
                                    palette_dismissed.set(true);
                                }
                                // `[webui] enter_key = "send"` — Enter submits, Shift+Enter is the
                                // newline. The historical behaviour and the default.
                                Key::Enter
                                    if enter_sends()
                                        && !e.modifiers().contains(Modifiers::SHIFT) =>
                                {
                                    e.prevent_default();
                                    submit.call(());
                                }
                                // `enter_key = "newline"` — Ctrl/Cmd+Enter is the deliberate send.
                                // Plain Enter deliberately matches *no* arm below, so it falls to
                                // the browser's own newline and this handler never submits. That is
                                // the point of the setting: on a phone Enter is the easiest key to
                                // hit and a mis-send cannot be taken back, so in this mode nothing
                                // reachable by one keypress can send.
                                Key::Enter
                                    if !enter_sends()
                                        && (e.modifiers().contains(Modifiers::CONTROL)
                                            || e.modifiers().contains(Modifiers::META)) =>
                                {
                                    e.prevent_default();
                                    submit.call(());
                                }
                                _ => {}
                            }
                        },
                    }
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
/// The one-line header for a tool-result block: the daemon's own summary line when present,
/// falling back to a neutral label, with a trailing colon trimmed ("RESULT (Succeeded):" reads
/// as "RESULT (Succeeded)").
fn tool_block_label(content: &str) -> String {
    content
        .lines()
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .unwrap_or("Tool result")
        .trim_end_matches(':')
        .to_string()
}

#[component]
fn ToolBlock(content: String) -> Element {
    let mut expanded = use_signal(|| false);

    // The daemon's first line is already a summary ("RESULT (Succeeded):"). Reuse it as the header
    // rather than inventing one, falling back only if it is empty so the header is never blank.
    let label = tool_block_label(&content);
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

/// `{}` and `null` are the JSON spellings of "no arguments", and the empty string is the
/// trimmed version of the same idea — all three render as nothing.
fn clean_args(args: &str) -> String {
    if args.is_empty() || args == "{}" || args == "null" {
        String::new()
    } else {
        args.to_string()
    }
}

/// The parenthesized argument text shown next to a tool name. Empty and JSON-"empty" args show
/// nothing at all — `tool(clean()_up)` is noise, `tool` is the same information.
fn args_display(args: &str) -> String {
    if clean_args(args).is_empty() {
        String::new()
    } else {
        format!("({args})")
    }
}

#[component]
fn ThinkingStepRow(step: ThinkingStep) -> Element {
    let status_cls = match step.ok {
        None => "thinking-step pending",
        Some(true) => "thinking-step ok",
        Some(false) => "thinking-step err",
    };

    let args_text = args_display(&step.tool_args);

    let mark = match step.ok {
        None => "\u{23F3}",
        Some(true) => "\u{2713}",
        Some(false) => "\u{2717}",
    };

    let name_text = format!("\u{1F527} {}{}", step.tool_name, args_text);

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

/// Put the caret back in the chat box after tapping a palette row.
///
/// The row's `onmousedown` already prevents the blur on desktop, but a touch tap on a phone can
/// still take focus away — and being dropped out of the input right after asking for a completion is
/// the opposite of helpful.
#[cfg(target_arch = "wasm32")]
fn focus_chat_input() {
    use wasm_bindgen::JsCast;

    let Some(el) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("chat-input"))
        .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return;
    };
    let _ = el.focus();
}

#[cfg(not(target_arch = "wasm32"))]
fn focus_chat_input() {}

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

/// The `/api/chat/stream` URL for one turn.
///
/// Deliberately **not** `cfg(wasm32)` and deliberately pure: this is where a request parameter goes
/// missing, and on the wasm-only path that can only be found by deploying and watching what the
/// daemon does. `message` arrives already percent-encoded, since the caller needs it anyway.
///
/// The shape that matters: `session` / `incognito` / `profile` are mutually exclusive — the first
/// says *which* conversation, the other two say how to open one — while `model` is orthogonal to all
/// three and appends to whichever applied.
fn stream_url(
    api_base: &str,
    encoded_message: &str,
    session: Option<&str>,
    incognito: bool,
    pending_profile: Option<&str>,
    pending_model: Option<&str>,
) -> String {
    let mut url = match session {
        Some(id) => format!("{api_base}/api/chat/stream?message={encoded_message}&session={id}"),
        // `true`, not `1`: axum's `Query` deserializes a bool through `FromStr`, which accepts only
        // "true"/"false". `incognito=1` fails the whole extraction, so the request would 400 and the
        // chat would simply not answer — a loud failure, but for a silly reason.
        None if incognito => {
            format!("{api_base}/api/chat/stream?message={encoded_message}&incognito=true")
        }
        // A profile picked before the first message has nowhere to be applied yet — the session it
        // scopes does not exist. Carrying it on the request that *creates* the session is what makes
        // the first turn run under it, which for a "basic chat" profile is the turn that matters.
        None if pending_profile.is_some() => format!(
            "{api_base}/api/chat/stream?message={encoded_message}&profile={}",
            urlencoding::encode(pending_profile.unwrap_or_default())
        ),
        None => format!("{api_base}/api/chat/stream?message={encoded_message}"),
    };
    // Appended rather than folded into the match above, which is the whole point: as a fifth arm it
    // would be dropped silently whenever a session id or a profile was also present — and a model
    // that goes missing does not fail, it just answers on the wrong one.
    if let Some(model) = pending_model.filter(|m| !m.is_empty()) {
        url.push_str(&format!("&model={}", urlencoding::encode(model)));
    }
    url
}

/// Truncate a conversation title to 60 bytes, walking back to a char boundary so the cut never
/// lands mid-codepoint (any curly quote, em-dash, etc.), and appending an ellipsis.
///
/// Bytes, not chars, because this is a display cap; the walk-back is what makes the byte cut
/// safe for non-ASCII text.
fn truncate_title(text: &str) -> String {
    let t = text.trim();
    if t.len() > 60 {
        let mut cut = 57;
        while cut > 0 && !t.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}…", &t[..cut])
    } else {
        t.to_string()
    }
}

#[cfg(target_arch = "wasm32")]
fn open_stream(
    api_base: &str,
    message: &str,
    targets: StreamTargets,
    api_base_for_title: &str,
    incognito: bool,
    // A profile chosen before this conversation existed. Applied by the request that creates it.
    pending_profile: Option<String>,
    // A model chosen for this turn. Unlike the profile, applies whether or not the conversation
    // already exists — see `ChatRequest::model`.
    pending_model: Option<String>,
) {
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
    );
}

/// Rejoin a turn already running for `conv_id`, without sending anything.
///
/// The reload path. The daemon replays what has already happened and then continues live, so the
/// answer being written while the page was gone still lands on screen. Never opens an incognito
/// session: attaching is by definition to a conversation that already exists.
#[cfg(target_arch = "wasm32")]
fn attach_stream(api_base: &str, conv_id: &str, targets: StreamTargets, api_base_for_title: &str) {
    let url = format!("{api_base}/api/conversations/{conv_id}/attach");
    connect_stream(
        &url,
        api_base,
        targets,
        api_base_for_title,
        false,
        Some(conv_id.to_string()),
    );
}

/// Open an SSE connection at `url` and wire it into `targets`. Shared by the send path and the
/// reattach path, so a turn renders identically however this client came to be watching it.
#[cfg(target_arch = "wasm32")]
fn connect_stream(
    url: &str,
    api_base: &str,
    targets: StreamTargets,
    api_base_for_title: &str,
    opened_incognito: bool,
    // Conversation to re-read from the store if this stream ends badly. Set for an *attach*, where
    // the answer may already be on disk and the stream failing says nothing about the turn.
    reconcile: Option<String>,
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
                    let clean = clean_args(&args_preview);
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
                        messages.set(loaded.messages);
                    }
                });
            }
        });
        source.set_onerror(Some(on_err.as_ref().unchecked_ref()));
        on_err.forget();
    }
}

#[cfg(test)]
mod stream_url_tests {
    use super::stream_url;

    const BASE: &str = "http://d";

    fn url(session: Option<&str>, model: Option<&str>) -> String {
        stream_url(BASE, "hi", session, false, None, model)
    }

    /// The regression this function was extracted for. A model picked for an existing conversation
    /// has to survive alongside `session`; when the two were arms of one match it could not, and the
    /// symptom was a turn quietly answering on the wrong model rather than any kind of error.
    #[test]
    fn a_model_survives_a_session_id() {
        let u = url(Some("01ABC"), Some("openai/gpt-5"));
        assert!(u.contains("&session=01ABC"), "{u}");
        assert!(u.contains("&model=openai%2Fgpt-5"), "{u}");
    }

    /// The case that caused the live failure: no conversation yet, so the pick rides the request
    /// that creates one instead of going anywhere near the daemon-wide default.
    #[test]
    fn a_model_rides_the_request_that_creates_the_conversation() {
        let u = url(None, Some("z-ai/glm-4.5-air"));
        assert!(!u.contains("session="), "{u}");
        assert!(u.contains("&model=z-ai%2Fglm-4.5-air"), "{u}");
    }

    #[test]
    fn a_model_survives_incognito_and_a_profile() {
        let inc = stream_url(BASE, "hi", None, true, None, Some("m/1"));
        assert!(
            inc.contains("incognito=true") && inc.contains("&model=m%2F1"),
            "{inc}"
        );
        let prof = stream_url(BASE, "hi", None, false, Some("basic"), Some("m/1"));
        assert!(
            prof.contains("profile=basic") && prof.contains("&model=m%2F1"),
            "{prof}"
        );
    }

    /// Absent and empty both mean "say nothing", so the daemon falls through to its own precedence
    /// rather than being handed a blank slug to resolve.
    #[test]
    fn no_model_means_no_parameter() {
        assert!(!url(Some("01ABC"), None).contains("model="));
        assert!(!url(Some("01ABC"), Some("")).contains("model="));
    }

    /// `session` names an existing conversation; `incognito` and `profile` describe how to open a new
    /// one. Asserted so the exclusivity survives someone appending a parameter the way `model` is.
    #[test]
    fn a_session_id_suppresses_the_creation_only_parameters() {
        let u = stream_url(BASE, "hi", Some("01ABC"), true, Some("basic"), None);
        assert!(u.contains("&session=01ABC"), "{u}");
        assert!(!u.contains("incognito") && !u.contains("profile"), "{u}");
    }
}

#[cfg(test)]
mod chat_msg_tests {
    use super::*;

    /// The role mapping is a *translation* — the wire's vocabulary to the renderer's — and unknown
    /// wire roles must not break rendering: they read as user bubbles rather than nothing.
    #[test]
    fn wire_roles_map_to_bubble_roles() {
        for (wire, expected) in [
            ("assistant", "assistant"),
            ("tool", "tool"),
            ("system", "system"),
            ("user", "user"),
            ("something-new", "user"),
        ] {
            let msg = ChatMsg::from_wire(&ChatMessage {
                role: wire.to_string(),
                content: "x".to_string(),
                tool_calls: None,
                tool_call_id: None,
                model: None,
            });
            assert_eq!(msg.role, expected, "wire role {wire:?}");
        }
    }

    /// History never carries thinking steps (those exist only on the live SSE stream), so the wire
    /// decoder must not invent any.
    #[test]
    fn wire_messages_carry_no_thinking_steps() {
        let msg = ChatMsg::from_wire(&ChatMessage {
            role: "assistant".to_string(),
            content: "hi".to_string(),
            tool_calls: None,
            tool_call_id: None,
            model: None,
        });
        assert!(msg.thinking_steps.is_empty());
        assert_eq!(msg.content, "hi");
    }

    /// The three constructors set the role that each message kind renders under — a user message
    /// typed in this tab, an assistant turn, a stream failure.
    #[test]
    fn constructors_set_the_role() {
        assert_eq!(ChatMsg::new_user("q".into()).role, "user");
        assert_eq!(ChatMsg::new_assistant("a".into()).role, "assistant");
        assert_eq!(ChatMsg::new_error("e".into()).role, "error");
        for msg in [
            ChatMsg::new_user("q".into()),
            ChatMsg::new_assistant("a".into()),
            ChatMsg::new_error("e".into()),
        ] {
            assert!(
                msg.thinking_steps.is_empty(),
                "constructors must not add steps"
            );
        }
    }

    /// Short titles pass through unchanged (and trimmed); only over-60-byte titles are cut.
    #[test]
    fn short_titles_pass_through() {
        assert_eq!(truncate_title("What is a database?"), "What is a database?");
        assert_eq!(truncate_title("  padded  "), "padded");
    }

    /// The 60-byte cap is a display limit; crossing it appends the ellipsis that tells the reader
    /// the row is abbreviated.
    #[test]
    fn long_titles_are_cut_and_elided() {
        let long = "x".repeat(100);
        let out = truncate_title(&long);
        assert!(out.ends_with('…'), "{out}");
        assert!(out.len() < 100);
        // 57 chars + a 3-byte ellipsis: the visible name is exactly the capped window.
        assert_eq!(out, format!("{}…", "x".repeat(57)));
    }

    /// The regression the char-boundary walk exists for: a multi-byte title whose 57th byte lands
    /// mid-codepoint must not panic the sidebar's title write. Uses 4-byte chars (57 % 4 ≠ 0) so
    /// the naive byte cut would genuinely land mid-char — 3-byte CJK would put the boundary exactly
    /// at 57 and pass both ways.
    #[test]
    fn long_titles_cut_on_char_boundaries() {
        // 20 four-byte emoji = 80 bytes; a naive 57-byte cut would split the 15th char.
        let wide = "😀".repeat(20);
        let out = truncate_title(&wide);
        assert!(wide.starts_with(out.trim_end_matches('…')));
        assert!(
            out.is_char_boundary(out.len()),
            "output must be valid UTF-8 at the cut"
        );
    }

    /// The JSON spellings of "no arguments" and the empty string all render as nothing.
    #[test]
    fn empty_args_render_as_nothing() {
        assert_eq!(clean_args(""), "");
        assert_eq!(clean_args("{}"), "");
        assert_eq!(clean_args("null"), "");
        assert_eq!(args_display(""), "");
        assert_eq!(args_display("{}"), "");
        assert_eq!(args_display("null"), "");
    }

    #[test]
    fn real_args_are_kept_and_parenthesized() {
        assert_eq!(clean_args("path=/tmp/x"), "path=/tmp/x");
        assert_eq!(args_display("path=/tmp/x"), "(path=/tmp/x)");
        assert_eq!(args_display("{ \"a\": 1 }"), "({ \"a\": 1 })");
    }

    /// The tool block's header is the daemon's own summary line, colon trimmed; empty content
    /// falls back to a neutral label rather than a blank header.
    #[test]
    fn tool_block_header_uses_the_daemon_summary_line() {
        assert_eq!(
            tool_block_label("RESULT (Succeeded):\nwrote 12 notes\n"),
            "RESULT (Succeeded)"
        );
        assert_eq!(tool_block_label("  RESULT (Failed):  "), "RESULT (Failed)");
        assert_eq!(tool_block_label("\n\nbody without a header"), "Tool result");
        assert_eq!(tool_block_label(""), "Tool result");
    }
}
