//! Prompt extraction and event rendering tests (moved verbatim from main.rs).

#![allow(unused_imports)]

use super::*;
use crate::provider::catalog_model_ids;
use liberado_provider::MockProvider;
use tempfile::TempDir;

use super::test_support::*;
/// Independent P4.3 oracle. The fixture uses the ACP wire shapes, not shapes inferred from an
/// implementation: embedded text/blob content is nested under `resource`, while
/// `resource_link`, `image`, and `audio` are top-level content blocks.
#[test]
fn p4_3_acceptance_uses_exact_acp_wire_shapes() {
    let cases: Value =
        serde_json::from_str(include_str!("../tests/fixtures/p4_3_prompt_blocks.json"))
            .expect("valid P4.3 fixture");
    for case in cases.as_array().expect("fixture case array") {
        let name = case["name"].as_str().expect("case name");
        let result = extract_prompt_text(&case["params"]);
        if let Some(expected) = case.get("expected").and_then(Value::as_str) {
            assert_eq!(result.expect(name), expected, "{name}");
        } else {
            let expected_error = case["error"].as_str().expect("case error");
            assert_eq!(result.expect_err(name), expected_error, "{name}");
        }
    }
}

/// The whole point of advertising `embeddedContext: true`: embedded textual resources reach
/// the model with a source line above them, in prompt order, without dropping the rest.
#[test]
fn extract_prompt_preserves_mixed_block_order() {
    let params = json!({
        "sessionId": "s1",
        "prompt": [
            { "type": "text", "text": "first" },
            {
                "type": "resource",
                "resource": {
                    "uri": "file:///notes/plan.md",
                    "mimeType": "text/markdown",
                    "text": "embedded body"
                }
            },
            { "type": "text", "text": "last" }
        ]
    });
    let out = extract_prompt_text(&params).unwrap();
    let first = out.find("first").unwrap();
    let marker = out.find("[resource: file:///notes/plan.md").unwrap();
    let body = out.find("embedded body").unwrap();
    let last = out.find("last").unwrap();
    assert!(
        first < marker && marker < body && body < last,
        "block order must survive extraction: {out:?}"
    );
}

#[test]
fn extract_prompt_renders_embedded_text_resource() {
    let params = json!({
        "prompt": [
            {
                "type": "resource",
                "resource": {
                    "uri": "file:///notes/plan.md",
                    "mimeType": "text/markdown",
                    "text": "# Plan\n\nDo the thing."
                }
            }
        ]
    });
    assert_eq!(
        extract_prompt_text(&params).unwrap(),
        "[resource: file:///notes/plan.md (text/markdown)]\n# Plan\n\nDo the thing."
    );
}

/// A resource link carries no embedded text, so it renders as a concise source marker.
#[test]
fn extract_prompt_marks_resource_link() {
    let params = json!({
        "prompt": [
            {
                "type": "resource_link",
                "uri": "file:///src/lib.rs",
                "name": "lib.rs",
                "mimeType": "text/x-rust"
            }
        ]
    });
    assert_eq!(
        extract_prompt_text(&params).unwrap(),
        "[resource: file:///src/lib.rs | lib.rs (text/x-rust)]"
    );
}

/// A resource link without optional MIME data still identifies its required name and URI.
#[test]
fn extract_prompt_marks_resource_link_without_mime() {
    let params = json!({
        "prompt": [
            { "type": "resource_link", "uri": "file:///data.csv", "name": "data.csv" }
        ]
    });
    assert_eq!(
        extract_prompt_text(&params).unwrap(),
        "[resource: file:///data.csv | data.csv]"
    );
}

/// A binary blob is embedded but must stay metadata-only: the base64 payload is never decoded
/// into text, only the source marker is rendered.
#[test]
fn extract_prompt_keeps_binary_resource_metadata_only() {
    let params = json!({
        "prompt": [
            {
                "type": "resource",
                "resource": {
                    "uri": "file:///data.bin",
                    "mimeType": "application/octet-stream",
                    "blob": "SGVsbG8="
                }
            }
        ]
    });
    let out = extract_prompt_text(&params).unwrap();
    assert_eq!(
        out,
        "[resource: file:///data.bin (application/octet-stream)]"
    );
    assert!(
        !out.contains("SGVsbG8="),
        "base64 payload must not reach the model"
    );
}

/// Image and audio blocks are rejected from the text stream entirely; a prompt made only of
/// them has no usable text and must fail as before.
#[test]
fn extract_prompt_rejects_media_only_prompt() {
    let params = json!({
        "prompt": [
            { "type": "image", "data": "aW1hZ2U=", "mimeType": "image/png" },
            { "type": "audio", "data": "YXVkaW8=", "mimeType": "audio/mp3" }
        ]
    });
    assert_eq!(
        extract_prompt_text(&params).unwrap_err(),
        "prompt contained no text content"
    );
}

/// When media blocks sit next to real text, they are dropped, never decoded.
#[test]
fn extract_prompt_media_blocks_never_decode_to_text() {
    let params = json!({
        "prompt": [
            { "type": "text", "text": "keep me" },
            { "type": "image", "data": "aW1hZ2U=", "mimeType": "image/png" },
            { "type": "audio", "data": "YXVkaW8=", "mimeType": "audio/mp3" }
        ]
    });
    let out = extract_prompt_text(&params).unwrap();
    assert_eq!(out, "keep me");
    assert!(!out.contains("aW1hZ2U=") && !out.contains("YXVkaW8="));
}

/// Whitespace-only blocks must still fail; embedded-context support must not make an empty
/// prompt look usable.
#[test]
fn extract_prompt_empty_text_still_fails() {
    let params = json!({
        "prompt": [
            { "type": "text", "text": "   " }
        ]
    });
    assert_eq!(
        extract_prompt_text(&params).unwrap_err(),
        "prompt contained no text content"
    );
}

#[test]
fn tool_call_ids_pair_start_and_finish() {
    let mut pending = Vec::new();
    let start_a = push_tool_call_id(&mut pending, "read_file");
    let start_b = push_tool_call_id(&mut pending, "run_command");
    assert_ne!(start_a, start_b);
    assert_eq!(pop_tool_call_id(&mut pending, "run_command"), start_b);
    assert_eq!(pop_tool_call_id(&mut pending, "read_file"), start_a);
    assert!(pending.is_empty());
}

#[test]
fn tool_call_ids_pair_same_name_lifo() {
    let mut pending = Vec::new();
    let first = push_tool_call_id(&mut pending, "read_file");
    let second = push_tool_call_id(&mut pending, "read_file");
    // Finish order matches LIFO (inner tool completes first).
    assert_eq!(pop_tool_call_id(&mut pending, "read_file"), second);
    assert_eq!(pop_tool_call_id(&mut pending, "read_file"), first);
}

#[test]
fn tool_call_id_pairing_is_required_for_paseo_ui() {
    // Mutation guard: if start and finish minted independent ids (old bug), this fails.
    let mut pending = Vec::new();
    let started = push_tool_call_id(&mut pending, "list_files");
    let finished = pop_tool_call_id(&mut pending, "list_files");
    assert_eq!(
        started, finished,
        "Paseo indexes tool UI by toolCallId; start and finish must share one id"
    );
}

/// render_coding_event maps tool activity to tool_call entries and everything else to text.
#[test]
fn render_coding_event_emits_text_and_tool_entries() {
    use liberado_session::{SessionEvent, SessionEventKind};

    let sink = CaptureSink {
        lines: std::sync::Mutex::new(Vec::new()),
    };
    let mk = |kind| SessionEvent::new("s", kind);

    render_coding_event(
        &sink,
        "s",
        &mk(SessionEventKind::Token { text: "hi".into() }),
        &mut Vec::new(),
    )
    .unwrap();
    let mut pending = Vec::new();
    render_coding_event(
        &sink,
        "s",
        &mk(SessionEventKind::ToolStarted {
            name: "read".into(),
            args_preview: "\"a.json\"".into(),
        }),
        &mut pending,
    )
    .unwrap();
    assert!(!pending.is_empty(), "an open tool call must be tracked");
    render_coding_event(
        &sink,
        "s",
        &mk(SessionEventKind::ToolFinished {
            name: "read".into(),
            ok: true,
            result_preview: "ok".into(),
        }),
        &mut pending,
    )
    .unwrap();
    assert!(pending.is_empty(), "a finished tool call must be popped");
    render_coding_event(
        &sink,
        "s",
        &mk(SessionEventKind::ValidationFinished {
            ok: false,
            summary: "tests broke".into(),
        }),
        &mut Vec::new(),
    )
    .unwrap();

    let lines = sink.lines.lock().unwrap();
    let updates: Vec<String> = lines
        .iter()
        .map(|(_, p)| {
            p["update"]["sessionUpdate"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert!(
        updates.contains(&"agent_message_chunk".into()),
        "tokens and validation must render as text chunks: {updates:?}"
    );
    assert!(
        lines.iter().any(|(_, p)| {
            p["update"]["content"]["text"]
                .as_str()
                .is_some_and(|t| t.contains("hi"))
        }),
        "the token text must be emitted"
    );
    assert!(
        updates.contains(&"tool_call".into()) && updates.contains(&"tool_call_update".into()),
        "tool start/finish must render as tool entries: {updates:?}"
    );
}

/// Every text-shaped pack event must render its marker. Deleting a match arm would
/// otherwise silence that event on the ACP wire with no test the wiser.
#[test]
fn each_text_event_renders_its_marker() {
    use liberado_session::{SessionEvent, SessionEventKind as K};
    let cases: Vec<(SessionEvent, &str)> = vec![
        (
            SessionEvent::new(
                "s",
                K::FileChanged {
                    path: "src/lib.rs".into(),
                    change: "modified".into(),
                },
            ),
            "`modified` src/lib.rs",
        ),
        (
            SessionEvent::new(
                "s",
                K::Progress {
                    message: "step 2".into(),
                },
            ),
            "_step 2_",
        ),
        (
            SessionEvent::new(
                "s",
                K::LoopGuard {
                    guard: "same-diff".into(),
                    action: "pause".into(),
                },
            ),
            "**guard** same-diff -> pause",
        ),
        (
            SessionEvent::new(
                "s",
                K::CriticVerdict {
                    reviewer: "fresh-eyes".into(),
                    kind: "fresh".into(),
                    approved: false,
                    issues: vec!["test X binds wrong".into()],
                    coerced: false,
                },
            ),
            "**fresh-eyes** rejected (test X binds wrong)",
        ),
        (
            SessionEvent::new(
                "s",
                K::RoleStarted {
                    role: "implementer".into(),
                    model: "m1".into(),
                },
            ),
            "_implementer (m1)_",
        ),
        (
            SessionEvent::new(
                "s",
                K::ValidationFinished {
                    ok: true,
                    summary: "all green".into(),
                },
            ),
            "**validation passed:** all green",
        ),
    ];
    for (event, needle) in cases {
        let sink = CaptureSink {
            lines: std::sync::Mutex::new(Vec::new()),
        };
        emit_text_event(&sink, "s", &event).expect("rendering must not fail");
        let lines = sink.lines.lock().unwrap();
        assert_eq!(lines.len(), 1, "{needle}");
        let text = lines[0].1["update"]["content"]["text"]
            .as_str()
            .unwrap_or("");
        assert!(text.contains(needle), "expected {needle:?} in {text:?}");
    }
}

#[test]
fn extract_prompt_drops_image_and_audio_blocks_without_error() {
    // The bridge advertises image/audio false. Their dedicated arm drops every such block —
    // even one carrying a uri — before the unknown-type fallback could render a marker.
    let media_only = json!({
        "prompt": [
            { "type": "image", "data": "aGVsbG8=" },
            { "type": "audio", "data": "aGVsbG8=" }
        ]
    });
    let err =
        extract_prompt_text(&media_only).expect_err("media with no text leaves nothing to send");
    assert!(err.contains("no text content"), "{err}");

    let mixed = json!({
        "prompt": [
            { "type": "text", "text": "real words" },
            { "type": "image", "data": "aGVsbG8=" },
            { "type": "image", "uri": "file:///img.png", "name": "img.png" }
        ]
    });
    let out = extract_prompt_text(&mixed).unwrap();
    assert_eq!(
        out, "real words",
        "media contributes neither payload nor marker"
    );
}
