//! Split from `chat.rs` for module-health boundaries.

use super::stream_url;
use super::*;

const BASE: &str = "http://d";

fn url(session: Option<&str>, model: Option<&str>) -> String {
    stream_url(BASE, "hi", session, false, None, model)
}

#[test]
fn a_palette_tap_runs_the_picked_command_not_the_typed_prefix() {
    assert_eq!(
        submission_text("/pro", 0, false, Some("/profile")),
        "/profile"
    );
    assert_eq!(submission_text("/mo", 0, false, Some("/model")), "/model");
}

#[test]
fn keyboard_submit_still_accepts_the_selected_completion() {
    assert_eq!(submission_text("/hel", 0, false, None), "/help");
    assert_eq!(submission_text("  hello  ", 0, false, None), "hello");
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

/// The history effect must not wipe a first turn just because it has no conversation id yet.
/// That is the Silent Send bug: stream fail → `sending` false → `active_conv_id` still None →
/// the optimistic user bubble vanished. A ghost (incognito) also keeps its transcript.
#[test]
fn a_first_turn_without_a_session_keeps_the_local_transcript() {
    assert!(!clear_transcript_on_missing_id(false, false));
    assert!(!clear_transcript_on_missing_id(false, true));
    assert!(
        clear_transcript_on_missing_id(true, false),
        "leaving a saved conversation should still blank"
    );
    assert!(!clear_transcript_on_missing_id(true, true));
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
