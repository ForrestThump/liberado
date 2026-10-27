//! Composer model chip.
//!
//! The label follows the same order the daemon uses for a turn, as far as this
//! client can see it:
//!
//! 1. A model picked in this window and not yet sent.
//! 2. The active session profile's pinned model (`GET /api/profiles`, field `model`).
//! 3. The model stamped on the latest user or assistant message.
//! 4. The daemon's current model (`GET /api/models`, field `current`).
//!
//! "Auto" is only step 4 when the daemon reports no current model. A missing
//! response stays "…" so a slow catalog cannot claim the chat is unset.

use dioxus::prelude::*;
use serde::Deserialize;

use chat_client_contract::{ChatMessage, ConversationHistoryResponse, ModelsResponse};

const AUTO: &str = "Auto";
const LOADING: &str = "\u{2026}";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HeldState {
    pub session: Option<String>,
    pub model: Option<String>,
    pub was_sending: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum History<'a> {
    /// No conversation is open, so there is no stamp to wait for.
    NoConversation,
    /// The conversation's history has not arrived. Do not guess with the daemon model.
    Loading,
    Ready(Option<&'a str>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Daemon<'a> {
    Unknown,
    Unset,
    Set(&'a str),
}

pub(crate) struct ResolveInput<'a> {
    pub picked: Option<&'a str>,
    pub profile_name: Option<&'a str>,
    pub profiles_loaded: bool,
    pub profiles: &'a [(String, Option<String>)],
    pub history: History<'a>,
    pub daemon: Daemon<'a>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChipText {
    Loading,
    Auto,
    Model(String),
}

impl ChipText {
    pub(crate) fn label(&self) -> String {
        match self {
            ChipText::Loading => LOADING.to_string(),
            ChipText::Auto => AUTO.to_string(),
            ChipText::Model(id) => short_model_name(id),
        }
    }

    pub(crate) fn title(&self) -> String {
        match self {
            ChipText::Loading => "Loading the model for this chat.".to_string(),
            ChipText::Auto => "No model is set.".to_string(),
            ChipText::Model(id) => id.clone(),
        }
    }
}

pub(crate) fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

/// The last path segment. `minimax/MiniMax-M3` reads as `MiniMax-M3`.
pub(crate) fn short_model_name(id: &str) -> String {
    let trimmed = id.trim();
    trimmed
        .rsplit('/')
        .next()
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}

pub(crate) fn pin_for<'a>(name: &str, profiles: &'a [(String, Option<String>)]) -> Option<&'a str> {
    profiles
        .iter()
        .find(|(row, _)| row == name)
        .and_then(|(_, model)| nonempty(model.as_deref()))
}

pub(crate) fn chip_text(input: &ResolveInput<'_>) -> ChipText {
    if let Some(name) = nonempty(input.picked) {
        return ChipText::Model(name.to_string());
    }
    if let Some(text) = profile_text(input) {
        return text;
    }
    if let Some(text) = history_text(input.history) {
        return text;
    }
    daemon_text(input.daemon)
}

/// `Some` stops the chain: still loading, or the profile pins a model.
/// `None` means this profile does not pin one, so the stamp and the daemon model still apply.
fn profile_text(input: &ResolveInput<'_>) -> Option<ChipText> {
    let name = input.profile_name?;
    if !input.profiles_loaded {
        return Some(ChipText::Loading);
    }
    pin_for(name, input.profiles).map(|pin| ChipText::Model(pin.to_string()))
}

fn history_text(history: History<'_>) -> Option<ChipText> {
    match history {
        History::Loading => Some(ChipText::Loading),
        History::Ready(stamp) => nonempty(stamp).map(|name| ChipText::Model(name.to_string())),
        History::NoConversation => None,
    }
}

fn daemon_text(daemon: Daemon<'_>) -> ChipText {
    match daemon {
        Daemon::Set(name) => match nonempty(Some(name)) {
            Some(name) => ChipText::Model(name.to_string()),
            None => ChipText::Loading,
        },
        Daemon::Unset => ChipText::Auto,
        Daemon::Unknown => ChipText::Loading,
    }
}

/// Keep a pick made before the conversation had an id. Drop it when the chat changes.
pub(crate) fn keep_across_session(prev: Option<&str>, next: Option<&str>) -> bool {
    prev == next || (prev.is_none() && next.is_some())
}

pub(crate) fn evolve_held(
    held: &HeldState,
    next_session: Option<&str>,
    pending: Option<&str>,
    sending: bool,
) -> HeldState {
    if let Some(model) = nonempty(pending) {
        return HeldState {
            session: next_session.map(str::to_string),
            model: Some(model.to_string()),
            was_sending: sending,
        };
    }
    if held.was_sending && !sending {
        return HeldState {
            session: next_session.map(str::to_string),
            model: None,
            was_sending: false,
        };
    }
    if keep_across_session(held.session.as_deref(), next_session) {
        HeldState {
            session: next_session.map(str::to_string),
            model: held.model.clone(),
            was_sending: sending,
        }
    } else {
        HeldState {
            session: next_session.map(str::to_string),
            model: None,
            was_sending: sending,
        }
    }
}

pub(crate) fn display_pick<'a>(
    pending: Option<&'a str>,
    held: &'a HeldState,
    session: Option<&str>,
) -> Option<&'a str> {
    if let Some(pick) = nonempty(pending) {
        return Some(pick);
    }
    if keep_across_session(held.session.as_deref(), session) {
        nonempty(held.model.as_deref())
    } else {
        None
    }
}

pub(crate) fn latest_stamp(messages: &[ChatMessage]) -> Option<String> {
    messages.iter().rev().find_map(stamp_of)
}

fn stamp_of(msg: &ChatMessage) -> Option<String> {
    if msg.role == "user" || msg.role == "assistant" {
        nonempty(msg.model.as_deref()).map(str::to_string)
    } else {
        None
    }
}

#[derive(Clone, Debug)]
struct LoadedChip {
    for_session: Option<String>,
    daemon: OwnedDaemon,
    profiles_loaded: bool,
    profiles: Vec<(String, Option<String>)>,
    history: OwnedHistory,
    pending_model: Option<String>,
}

#[derive(Clone, Debug)]
enum OwnedDaemon {
    Unknown,
    Unset,
    Set(String),
}

#[derive(Clone, Debug)]
enum OwnedHistory {
    NoConversation,
    Failed,
    Ready(Option<String>),
}

fn owned_daemon(current: Option<String>, ok: bool) -> OwnedDaemon {
    if !ok {
        return OwnedDaemon::Unknown;
    }
    match nonempty(current.as_deref()) {
        Some(name) => OwnedDaemon::Set(name.to_string()),
        None => OwnedDaemon::Unset,
    }
}

fn daemon_view(daemon: &OwnedDaemon) -> Daemon<'_> {
    match daemon {
        OwnedDaemon::Unknown => Daemon::Unknown,
        OwnedDaemon::Unset => Daemon::Unset,
        OwnedDaemon::Set(name) => Daemon::Set(name),
    }
}

fn history_view(history: &OwnedHistory) -> History<'_> {
    match history {
        OwnedHistory::NoConversation => History::NoConversation,
        OwnedHistory::Failed => History::Loading,
        OwnedHistory::Ready(stamp) => History::Ready(stamp.as_deref()),
    }
}

async fn load_daemon(api_base: &str) -> OwnedDaemon {
    let url = format!("{api_base}/api/models");
    let Ok(resp) = crate::http::get(&url).await else {
        return OwnedDaemon::Unknown;
    };
    if !resp.status().is_success() {
        return OwnedDaemon::Unknown;
    }
    match resp.json::<ModelsResponse>().await {
        Ok(body) => owned_daemon(body.current, true),
        Err(_) => OwnedDaemon::Unknown,
    }
}

#[derive(Debug, Deserialize)]
struct ProfileWire {
    name: String,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProfilesWire {
    #[serde(default)]
    profiles: Vec<ProfileWire>,
}

async fn load_profiles(api_base: &str) -> (bool, Vec<(String, Option<String>)>) {
    let url = format!("{api_base}/api/profiles");
    let Ok(resp) = crate::http::get(&url).await else {
        return (false, Vec::new());
    };
    if !resp.status().is_success() {
        return (false, Vec::new());
    }
    match resp.json::<ProfilesWire>().await {
        Ok(body) => (
            true,
            body.profiles
                .into_iter()
                .map(|row| (row.name, row.model))
                .collect(),
        ),
        Err(_) => (false, Vec::new()),
    }
}

async fn load_history(api_base: &str, session: &str) -> (OwnedHistory, Option<String>) {
    let url = format!("{api_base}/api/conversations/{session}");
    let Ok(resp) = crate::http::get(&url).await else {
        return (OwnedHistory::Failed, None);
    };
    if !resp.status().is_success() {
        return (OwnedHistory::Failed, None);
    }
    match resp.json::<ConversationHistoryResponse>().await {
        Ok(body) => (
            OwnedHistory::Ready(latest_stamp(&body.messages)),
            body.pending_model.filter(|model| !model.trim().is_empty()),
        ),
        Err(_) => (OwnedHistory::Failed, None),
    }
}

async fn load_chip(api_base: String, session: Option<String>) -> LoadedChip {
    let daemon = load_daemon(&api_base).await;
    let (profiles_loaded, profiles) = load_profiles(&api_base).await;
    let (history, pending_model) = match session.as_deref() {
        Some(id) => load_history(&api_base, id).await,
        None => (OwnedHistory::NoConversation, None),
    };
    LoadedChip {
        for_session: session,
        daemon,
        profiles_loaded,
        profiles,
        history,
        pending_model,
    }
}

/// Local pick wins. Otherwise show the pending model stored on the conversation.
pub(crate) fn shown_pick<'a>(local: Option<&'a str>, stored: Option<&'a str>) -> Option<&'a str> {
    nonempty(local).or_else(|| nonempty(stored))
}

fn text_for<'a>(
    picked: Option<&'a str>,
    profile_name: Option<&'a str>,
    session: Option<&str>,
    row: Option<&'a LoadedChip>,
) -> ChipText {
    let Some(row) = row.filter(|row| row.for_session.as_deref() == session) else {
        return chip_text(&ResolveInput {
            picked,
            profile_name,
            profiles_loaded: false,
            profiles: &[],
            history: unloaded_history(session),
            daemon: Daemon::Unknown,
        });
    };
    chip_text(&ResolveInput {
        picked,
        profile_name,
        profiles_loaded: row.profiles_loaded,
        profiles: &row.profiles,
        history: history_view(&row.history),
        daemon: daemon_view(&row.daemon),
    })
}

fn unloaded_history(session: Option<&str>) -> History<'static> {
    if session.is_some() {
        History::Loading
    } else {
        History::NoConversation
    }
}

#[component]
pub fn ModelChip(
    api_base: String,
    pending: Signal<Option<String>>,
    profile: Signal<Option<String>>,
    session: Signal<Option<String>>,
    sending: Signal<bool>,
    on_open: EventHandler<()>,
) -> Element {
    let mut held = use_signal(HeldState::default);
    let loaded = use_resource(move || {
        let base = api_base.clone();
        let id = session();
        let _reload_after_turn = sending();
        async move { load_chip(base, id).await }
    });

    use_effect(move || {
        let next = evolve_held(
            &held(),
            session().as_deref(),
            pending().as_deref(),
            sending(),
        );
        if next != held() {
            held.set(next);
        }
    });

    let session_now = session();
    let pending_now = pending();
    let held_now = held();
    let stored_model = loaded
        .read()
        .as_ref()
        .and_then(|row| row.pending_model.clone());
    let local =
        display_pick(pending_now.as_deref(), &held_now, session_now.as_deref()).map(str::to_string);
    let picked = shown_pick(local.as_deref(), stored_model.as_deref());
    let text = text_for(
        picked,
        profile().as_deref(),
        session_now.as_deref(),
        loaded.read().as_ref(),
    );
    let label = text.label();
    let title = text.title();
    let aria = format!("Choose model, {label}");

    rsx! {
        button {
            class: "toolbar-mode-btn",
            r#type: "button",
            title: "{title}",
            "aria-label": "{aria}",
            onclick: move |evt: MouseEvent| {
                evt.prevent_default();
                on_open.call(());
            },
            span { class: "toolbar-mode-label", "{label}" }
            span { class: "toolbar-mode-caret", "▾" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, model: Option<&str>) -> ChatMessage {
        ChatMessage {
            role: role.into(),
            content: String::new(),
            tool_calls: None,
            tool_call_id: None,
            model: model.map(str::to_string),
            reasoning: None,
        }
    }

    fn profiles(rows: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
        rows.iter()
            .map(|(name, model)| ((*name).to_string(), model.map(str::to_string)))
            .collect()
    }

    fn base<'a>(profiles: &'a [(String, Option<String>)]) -> ResolveInput<'a> {
        ResolveInput {
            picked: None,
            profile_name: None,
            profiles_loaded: true,
            profiles,
            history: History::NoConversation,
            daemon: Daemon::Unset,
        }
    }

    #[test]
    fn chip_shows_the_daemon_model_and_auto_only_when_unset() {
        let rows = profiles(&[]);
        let mut input = base(&rows);
        input.daemon = Daemon::Set("MiniMax-M3");
        let text = chip_text(&input);
        assert_eq!(text.label(), "MiniMax-M3");
        assert_eq!(text.title(), "MiniMax-M3");

        input.daemon = Daemon::Set("minimax/MiniMax-M3");
        let text = chip_text(&input);
        assert_eq!(text.label(), "MiniMax-M3");
        assert_eq!(text.title(), "minimax/MiniMax-M3");

        input.daemon = Daemon::Unset;
        assert_eq!(chip_text(&input), ChipText::Auto);
        assert_eq!(chip_text(&input).label(), "Auto");
    }

    #[test]
    fn an_unknown_daemon_is_not_labeled_auto() {
        let rows = profiles(&[]);
        let mut input = base(&rows);
        input.daemon = Daemon::Unknown;
        assert_eq!(chip_text(&input), ChipText::Loading);
        input.daemon = Daemon::Set("   ");
        assert_eq!(chip_text(&input), ChipText::Loading);
    }

    #[test]
    fn profile_pin_and_session_stamp_beat_the_daemon_model() {
        let rows = profiles(&[("research", Some("profile-model")), ("plain", None)]);
        let mut input = base(&rows);
        input.daemon = Daemon::Set("MiniMax-M3");
        input.profile_name = Some("research");
        assert_eq!(chip_text(&input).label(), "profile-model");

        input.profile_name = Some("plain");
        input.history = History::Ready(Some("stamped-model"));
        assert_eq!(chip_text(&input).label(), "stamped-model");

        input.profile_name = None;
        assert_eq!(chip_text(&input).label(), "stamped-model");
    }

    #[test]
    fn a_stored_pending_model_shows_until_a_local_pick_replaces_it() {
        assert_eq!(shown_pick(None, Some("vendor/exact")), Some("vendor/exact"));
        assert_eq!(
            shown_pick(Some("request-model"), Some("vendor/exact")),
            Some("request-model")
        );
        assert_eq!(
            shown_pick(Some("   "), Some("vendor/exact")),
            Some("vendor/exact")
        );
    }

    #[test]
    fn a_pick_beats_the_profile_and_a_loading_session_hides_the_daemon_model() {
        let rows = profiles(&[("research", Some("profile-model"))]);
        let mut input = base(&rows);
        input.daemon = Daemon::Set("MiniMax-M3");
        input.profile_name = Some("research");
        input.picked = Some("picked-model");
        assert_eq!(chip_text(&input).label(), "picked-model");

        input.picked = None;
        input.profiles_loaded = false;
        assert_eq!(chip_text(&input), ChipText::Loading);

        input.profile_name = None;
        input.profiles_loaded = true;
        input.history = History::Loading;
        assert_eq!(
            chip_text(&input),
            ChipText::Loading,
            "a session whose stamp has not loaded must not show the daemon model"
        );
    }

    #[test]
    fn latest_stamp_uses_the_newest_user_or_assistant_model() {
        let messages = vec![
            msg("user", Some("old")),
            msg("tool", Some("tool-model")),
            msg("assistant", Some("MiniMax-M3")),
            msg("system", Some("system-model")),
        ];
        assert_eq!(latest_stamp(&messages).as_deref(), Some("MiniMax-M3"));
        assert_eq!(latest_stamp(&[msg("user", None)]), None);
    }

    #[test]
    fn held_pick_survives_the_first_session_id_and_clears_when_the_turn_ends() {
        let held = evolve_held(&HeldState::default(), None, Some("MiniMax-M3"), false);
        assert_eq!(held.model.as_deref(), Some("MiniMax-M3"));
        let during = evolve_held(&held, Some("01CHAT"), None, true);
        assert_eq!(during.model.as_deref(), Some("MiniMax-M3"));
        assert_eq!(
            display_pick(None, &during, Some("01CHAT")),
            Some("MiniMax-M3")
        );
        let done = evolve_held(&during, Some("01CHAT"), None, false);
        assert_eq!(done.model, None);
        let switched = evolve_held(&during, Some("01OTHER"), None, true);
        assert_eq!(switched.model, None);
    }
}
