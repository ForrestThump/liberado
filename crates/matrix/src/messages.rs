//! Event bodies for `m.room.message`, including edits.
//!
//! An edit carries `m.new_content` plus `m.relates_to` with `rel_type` `m.replace`.
//! The outer `body` is the plain fallback clients show when they do not understand edits.

use serde_json::{Value, json};

/// Plain text or a notice. Notices are used for the encrypted-room reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgKind {
    Text,
    Notice,
}

/// `m.room.message` content with a plain body and an HTML `formatted_body`.
pub fn message_content(kind: MsgKind, plain: &str, html: &str) -> Value {
    json!({
        "msgtype": msgtype(kind),
        "body": plain,
        "format": "org.matrix.custom.html",
        "formatted_body": html,
    })
}

/// Replacement content for a previous event.
pub fn edit_content(event_id: &str, plain: &str, html: &str) -> Value {
    json!({
        "msgtype": "m.text",
        "body": format!("* {plain}"),
        "format": "org.matrix.custom.html",
        "formatted_body": html,
        "m.new_content": message_content(MsgKind::Text, plain, html),
        "m.relates_to": {
            "rel_type": "m.replace",
            "event_id": event_id,
        }
    })
}

fn msgtype(kind: MsgKind) -> &'static str {
    match kind {
        MsgKind::Text => "m.text",
        MsgKind::Notice => "m.notice",
    }
}

/// Split `text` into pieces of at most `max_bytes`, on UTF-8 boundaries.
///
/// An empty input yields one empty piece so a send still produces an event.
pub fn byte_chunks(text: &str, max_bytes: usize) -> Vec<&str> {
    if text.is_empty() || max_bytes == 0 {
        return vec![text];
    }
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if rest.len() <= max_bytes {
            out.push(rest);
            break;
        }
        let end = chunk_end(rest, max_bytes);
        let (head, tail) = rest.split_at(end);
        out.push(head);
        rest = tail;
    }
    out
}

fn chunk_end(text: &str, max_bytes: usize) -> usize {
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    if end == 0 {
        text.chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(text.len())
    } else {
        end
    }
}

/// Matrix event body cap used by the channel. The homeserver allows about 65 KB.
pub const CHUNK_BYTES: usize = 30_000;

#[cfg(test)]
mod tests {
    use super::{CHUNK_BYTES, MsgKind, byte_chunks, edit_content, message_content};

    #[test]
    fn message_content_carries_html_and_the_msgtype() {
        let text = message_content(MsgKind::Text, "hi", "<p>hi</p>");
        assert_eq!(text["msgtype"], "m.text");
        assert_eq!(text["body"], "hi");
        assert_eq!(text["format"], "org.matrix.custom.html");
        assert_eq!(text["formatted_body"], "<p>hi</p>");

        let notice = message_content(MsgKind::Notice, "no", "<p>no</p>");
        assert_eq!(notice["msgtype"], "m.notice");
    }

    #[test]
    fn edit_content_points_at_the_original_event() {
        let edit = edit_content("$evt", "new", "<p>new</p>");
        assert_eq!(edit["body"], "* new");
        assert_eq!(edit["m.new_content"]["body"], "new");
        assert_eq!(edit["m.new_content"]["formatted_body"], "<p>new</p>");
        assert_eq!(edit["m.relates_to"]["rel_type"], "m.replace");
        assert_eq!(edit["m.relates_to"]["event_id"], "$evt");
    }

    #[test]
    fn chunks_keep_char_boundaries_and_the_cap() {
        let text = format!("a{}🙂", "b".repeat(CHUNK_BYTES));
        let parts = byte_chunks(&text, CHUNK_BYTES);
        assert!(parts.len() >= 2);
        assert!(parts.iter().all(|part| part.len() <= CHUNK_BYTES));
        assert_eq!(parts.concat(), text);
        assert_eq!(byte_chunks("", CHUNK_BYTES), vec![""]);
    }
}
