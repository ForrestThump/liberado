//! Pure rules for the Automations editor dock.
//!
//! The dock posts one singleton open, shows that session's cards, and sends
//! turns on `POST /api/chat`. These functions stay free of the browser so the
//! host tests can pin them.

use chat_client_contract::{ChatMessage, PendingApproval};

use super::super::chat::ChatMsg;
use super::super::chat::chat_reasoning::fold_thinking;
use super::super::chat_submission::bubble_role;

/// How far from the bottom still counts as following the latest editor row.
pub const EDITOR_SCROLL_SLACK_PX: i32 = 80;

/// Log label when the reader posts from the editor composer.
pub const SCROLL_TRIGGER_SEND: &str = "send";

/// Log label when the dock's interval reloads the transcript.
pub const SCROLL_TRIGGER_POLL: &str = "poll";

/// Log label when a render applies history without a send or a poll.
pub const SCROLL_TRIGGER_RENDER: &str = "render";

/// Log label when a live token grows the main transcript.
pub const SCROLL_TRIGGER_STREAM: &str = "stream";

/// How often the dock re-reads the transcript after Approve.
pub const EDITOR_POLL_MS: i32 = 2_000;

/// Body of the find-or-create `POST /api/conversations`.
///
/// `profile` and `title` stay off the body. The server ignores them when this
/// flag is set, and a second call returns the same id.
pub fn editor_open_body() -> serde_json::Value {
    serde_json::json!({ "automation_editor": true })
}

/// First open is 201. A later open of the same row is 200.
pub fn editor_open_accepted(status: u16) -> bool {
    status == 200 || status == 201
}

/// Session id from the conversation header the open route returns.
pub fn session_id_from_open(body: &serde_json::Value) -> Option<String> {
    let id = body.get("id")?.as_str()?.trim();
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

/// Body of `POST /api/chat` for one editor turn.
pub fn editor_chat_body(session_id: &str, message: &str) -> serde_json::Value {
    serde_json::json!({
        "message": message,
        "session": session_id,
    })
}

/// Reply text from a successful `POST /api/chat`.
pub fn reply_from_chat(body: &serde_json::Value) -> Option<String> {
    let reply = body.get("reply")?.as_str()?.trim();
    if reply.is_empty() {
        None
    } else {
        Some(reply.to_string())
    }
}

/// Error string from a JSON body, or a status line when the body has none.
pub fn error_from_body(body: &str, status: u16) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    let text = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(|error| error.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty());
    match text {
        Some(text) => text.to_string(),
        None => format!("Request failed (HTTP {status})"),
    }
}

/// Pending cards that belong to this editor session.
///
/// `GET /api/approvals/pending` is global. A card with no conversation, or
/// with a different one, stays out of this dock.
pub fn approvals_for_session<'a>(
    pending: &'a [PendingApproval],
    session_id: &str,
) -> Vec<&'a PendingApproval> {
    pending
        .iter()
        .filter(|card| card.conversation.as_deref() == Some(session_id))
        .collect()
}

/// Ids of the pending cards for this session, in list order.
pub fn pending_ids_for_session(pending: &[PendingApproval], session_id: &str) -> Vec<String> {
    approvals_for_session(pending, session_id)
        .into_iter()
        .map(|card| card.id.clone())
        .collect()
}

/// Trimmed composer text. Blank input does not send.
pub fn composer_message(raw: &str) -> Option<String> {
    let text = raw.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// Text the Send button may post. A busy dock or a missing session posts nothing.
pub fn begin_send(sending: bool, session_ready: bool, raw: &str) -> Option<String> {
    if sending || !session_ready {
        return None;
    }
    composer_message(raw)
}

/// Drop the system prompt. Keep user, assistant, tool, and error rows.
///
/// An Approve note and a refused write are assistant text. They stay so the
/// reader sees "the file is on disk" and a read-only refusal in the thread.
pub fn editor_transcript(messages: &[ChatMessage]) -> Vec<ChatMsg> {
    messages.iter().filter_map(editor_line).collect()
}

fn editor_line(message: &ChatMessage) -> Option<ChatMsg> {
    if message.role == "system" {
        return None;
    }
    let role = bubble_role(&message.role);
    let (content, reasoning) = fold_thinking(&message.content, message.reasoning.as_deref());
    Some(ChatMsg {
        role,
        content,
        thinking_steps: Vec::new(),
        reasoning,
    })
}

/// One assistant row when the history reload fails after a turn that did return text.
pub fn assistant_line(content: String) -> ChatMsg {
    ChatMsg {
        role: "assistant",
        content,
        thinking_steps: Vec::new(),
        reasoning: None,
    }
}

pub fn send_label(sending: bool) -> &'static str {
    if sending { "Sending" } else { "Send" }
}

pub fn send_is_disabled(sending: bool, session_missing: bool) -> bool {
    sending || session_missing
}

pub fn dock_class(no_pending: bool) -> &'static str {
    if no_pending {
        "automation-editor-dock"
    } else {
        "automation-editor-dock has-pending"
    }
}

/// Follow the latest row only when the reader is already there, or they just sent.
pub fn should_autoscroll(near_bottom: bool, user_sent: bool) -> bool {
    user_sent || near_bottom
}

/// True when the viewport sits within [`EDITOR_SCROLL_SLACK_PX`] of the content bottom.
pub fn editor_near_bottom(scroll_top: i32, client_height: i32, scroll_height: i32) -> bool {
    pixels_from_bottom(scroll_top, client_height, scroll_height) <= EDITOR_SCROLL_SLACK_PX
}

fn pixels_from_bottom(scroll_top: i32, client_height: i32, scroll_height: i32) -> i32 {
    let shown = scroll_top.max(0).saturating_add(client_height.max(0));
    scroll_height.max(0).saturating_sub(shown)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            model: None,
            reasoning: None,
        }
    }

    fn pending(id: &str, conversation: Option<&str>) -> PendingApproval {
        PendingApproval {
            id: id.to_string(),
            conversation: conversation.map(str::to_string),
            tool: Some("update_schedule".into()),
            zone: None,
            created_at: "2026-10-16T00:00:00Z".into(),
        }
    }

    #[test]
    fn the_open_body_is_only_the_singleton_flag() {
        let body = editor_open_body();
        assert_eq!(body, serde_json::json!({ "automation_editor": true }));
        assert!(body.get("profile").is_none());
        assert!(body.get("title").is_none());
        assert!(body.get("agent_creator").is_none());
        assert!(editor_open_accepted(200));
        assert!(editor_open_accepted(201));
        assert!(!editor_open_accepted(400));
    }

    #[test]
    fn the_open_response_yields_the_session_id() {
        let body = serde_json::json!({ "id": "01EDITOR", "title": "Automation Editor" });
        assert_eq!(session_id_from_open(&body).as_deref(), Some("01EDITOR"));
        assert_eq!(
            session_id_from_open(&serde_json::json!({ "id": "  " })),
            None
        );
        assert_eq!(session_id_from_open(&serde_json::json!({})), None);
    }

    #[test]
    fn the_chat_body_names_the_editor_session() {
        let body = editor_chat_body("01EDITOR", "move the morning brief to 7:15");
        assert_eq!(body["session"], "01EDITOR");
        assert_eq!(body["message"], "move the morning brief to 7:15");
        assert_eq!(
            reply_from_chat(&serde_json::json!({ "reply": " Filed. " })).as_deref(),
            Some("Filed.")
        );
        assert_eq!(reply_from_chat(&serde_json::json!({ "reply": "  " })), None);
    }

    #[test]
    fn approvals_keep_only_this_session() {
        let pending = vec![
            pending("mine", Some("01EDITOR")),
            pending("other", Some("01CHAT")),
            pending("loose", None),
        ];
        let kept = approvals_for_session(&pending, "01EDITOR");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, "mine");
        assert_eq!(
            pending_ids_for_session(&pending, "01EDITOR"),
            vec!["mine".to_string()]
        );
        assert!(approvals_for_session(&pending, "missing").is_empty());
    }

    #[test]
    fn a_refused_write_and_the_disk_note_stay_in_the_transcript() {
        let rows = editor_transcript(&[
            message("system", "You are the Automation Editor."),
            message("user", "move the morning brief"),
            message(
                "assistant",
                "topology.toml is read-only. The file was not changed.",
            ),
            message(
                "assistant",
                "The file is on disk. Cron and the schedules list still show the boot copy until the process is recreated.",
            ),
        ]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].role, "user");
        assert!(rows[1].content.contains("read-only"));
        assert!(rows[2].content.contains("The file is on disk"));
        assert!(rows.iter().all(|row| row.role != "system"));
    }

    #[test]
    fn blank_or_busy_input_does_not_send() {
        assert_eq!(composer_message("  7:15  ").as_deref(), Some("7:15"));
        assert_eq!(composer_message("   "), None);
        assert_eq!(begin_send(true, true, "hello"), None);
        assert_eq!(begin_send(false, false, "hello"), None);
        assert_eq!(
            begin_send(false, true, "  hello  ").as_deref(),
            Some("hello")
        );
        assert_eq!(send_label(false), "Send");
        assert_eq!(send_label(true), "Sending");
        assert!(send_is_disabled(false, true));
        assert!(!send_is_disabled(false, false));
        assert_eq!(dock_class(true), "automation-editor-dock");
        assert!(dock_class(false).contains("has-pending"));
        assert_eq!(
            error_from_body(r#"{"error":"read-only"}"#, 500),
            "read-only"
        );
        assert_eq!(error_from_body("nope", 503), "Request failed (HTTP 503)");
    }

    #[test]
    fn the_phone_dock_stays_inside_the_screen() {
        let css = include_str!("../styles/main.css");
        let dock = css
            .split(".automation-editor-dock {")
            .nth(1)
            .expect("dock rule");
        let dock = dock.split('}').next().unwrap_or(dock);
        assert!(dock.contains("min-width: 0"), "{dock}");
        assert!(dock.contains("overflow-x: hidden"), "{dock}");
        assert!(dock.contains("box-sizing: border-box"), "{dock}");
        let phone = css
            .split("/* automation-editor-phone */")
            .nth(1)
            .expect("phone dock block");
        let phone = phone
            .split("/* automation-editor-phone-end */")
            .next()
            .unwrap_or(phone);
        assert!(phone.contains("max-width: 768px"), "{phone}");
        assert!(phone.contains(".automation-editor-dock"), "{phone}");
        assert!(phone.contains("safe-area-inset-right"), "{phone}");
        assert!(phone.contains("overflow-x: hidden"), "{phone}");
        assert!(phone.contains("max-width: 100%"), "{phone}");
        assert!(phone.contains("100dvh"), "{phone}");
        assert!(phone.contains("safe-area-inset-bottom"), "{phone}");
    }

    #[test]
    fn scroll_triggers_name_send_poll_render_and_stream() {
        assert_eq!(SCROLL_TRIGGER_SEND, "send");
        assert_eq!(SCROLL_TRIGGER_POLL, "poll");
        assert_eq!(SCROLL_TRIGGER_RENDER, "render");
        assert_eq!(SCROLL_TRIGGER_STREAM, "stream");
    }

    #[test]
    fn autoscroll_follows_only_near_the_bottom_or_after_send() {
        assert!(should_autoscroll(true, false));
        assert!(should_autoscroll(false, true));
        assert!(!should_autoscroll(false, false));
        assert_eq!(EDITOR_SCROLL_SLACK_PX, 80);
    }

    #[test]
    fn near_bottom_is_an_eighty_pixel_window() {
        let slack = EDITOR_SCROLL_SLACK_PX;
        let at_slack = 800 - 100 - slack;
        assert!(editor_near_bottom(at_slack, 100, 800));
        assert!(!editor_near_bottom(at_slack - 1, 100, 800));
        assert!(editor_near_bottom(0, 100, 40));
        assert!(editor_near_bottom(-4, -1, -8));
    }

    #[test]
    fn the_dock_separator_is_a_bar_not_the_restart_footnote() {
        let editor = include_str!("automation_editor.rs");
        let css = include_str!("../styles/main.css");
        let footnote = ["Edits land in ", "topology.toml"].concat();
        assert!(!editor.contains(&footnote), "{editor}");
        assert!(editor.contains("automation-editor-split"));
        let bar = css
            .split(".automation-editor-split {")
            .nth(1)
            .expect("split rule");
        let bar = bar.split('}').next().unwrap_or(bar);
        assert!(bar.contains("height: 8px"), "{bar}");
        assert!(bar.contains("flex: none"), "{bar}");
        let composer = css
            .split(".automation-editor-composer {")
            .nth(1)
            .expect("composer rule");
        let composer = composer.split('}').next().unwrap_or(composer);
        assert!(composer.contains("flex: none"), "{composer}");
        let history = css
            .split(".automations-stack .automation-editor-transcript {")
            .nth(1)
            .expect("history rule");
        let history = history.split('}').next().unwrap_or(history);
        assert!(history.contains("min-height: 0"), "{history}");
    }
}
