//! Context compaction for long conversations (CH3 — see
//! `docs/roadmap/context-compaction-plan.md` for the design and the four-project research it
//! distills).
//!
//! The problem: [`ChatSessions`](crate::ChatSessions) rehydrates the *entire* conversation from
//! the store every turn, so a long enough history eventually exceeds the model's context window.
//! The fix, at the turn boundary: estimate the outgoing size, and over a configurable threshold
//! replace everything older than the last K user turns with a **rolling summary** — persisted as
//! a marker node in the session DAG so the compaction survives restarts and never re-triggers on
//! already-compacted history.
//!
//! Two halves:
//!
//! * **The trigger** — [`estimate_tokens`] (chars/4 × 1.3 safety factor, Kilo Code's constant; no
//!   tokenizer dependency) against [`CompactionConfig::trigger_tokens`].
//! * **The marker model** — a node authored [`COMPACTION_AUTHOR`] carrying the summary, followed
//!   by a verbatim re-append of the kept tail. The compacted *view* is then a contiguous suffix of
//!   the append-only log: system root → latest marker → tail. [`ChatSessions`](crate::ChatSessions)'
//!   load elides everything strictly between the root and the *latest* marker. Raw history is
//!   never deleted — the full transcript (marker included) still renders and stays searchable.
//!
//! Deliberate non-goals for this tier (captured in the plan doc): mid-turn prechecks inside the
//! executor loop, between-turn tool-result pruning, overflow-error-retry compaction, a manual
//! `/compact`, and a dedicated summarizer model.

use liberado_provider::{CompletionRequest, Message, Role};

/// The `Author::Named` identities compaction writes. Both live in the store crate because they are
/// a **read contract** shared across layers, not private kernel state: `ChatSessions::load` elides
/// everything between the system root and the latest [`COMPACTION_AUTHOR`] node, while every reader
/// that walks a raw leaf path must skip [`COMPACTION_TAIL_AUTHOR`] copies or double-count them.
/// They use the same pre-existing `Author::Named` seam as `append_note`'s `"goal-session"`
/// identity — additive, no store schema change.
///
/// CH3.1 removes the tail copies entirely (see
/// `docs/future-work/context-compaction-viewport-rearchitecture.md`); until then this is the seam.
pub use liberado_conversation_store::{COMPACTION_AUTHOR, COMPACTION_TAIL_AUTHOR};

/// First line of every marker message's content — identifies the bubble in rendered history and
/// tells the *next* summarizer (whose transcript opens with it) that this is a previous rolling
/// summary to carry forward, not ordinary conversation.
pub const SUMMARY_HEADER: &str = "[context compacted — summary of earlier conversation]";

/// Tunables for [`ChatSessions`](crate::ChatSessions)' automatic compaction. Defaults are sized
/// for a 64k-context chat model: trigger at 48k estimated tokens leaves ~16k of reserve for tool
/// schemas, the reply, and estimation slack. All fields have defaults; see
/// `docs/roadmap/context-compaction-plan.md` §Configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionConfig {
    /// Master switch. Default **on**: a reliability guard that is opt-in is off in practice
    /// (`failure-modes.md` §2). Disable only with a reason (e.g. an operator benchmarking raw
    /// recall).
    pub enabled: bool,
    /// Fire when the estimated tokens of history + the incoming user message exceed this.
    /// Absolute estimated tokens only — config-tier resolution (per-model `trigger_pct` /
    /// `trigger_tokens` overrides → face model window) happens in `liberado-server` before
    /// `ChatSessions::with_compaction`.
    pub trigger_tokens: u32,
    /// User turns (a user message and everything up to the next one) kept **verbatim** after the
    /// summary. Anchored on user messages so tool-call/result pairs can never be split (OpenClaw's
    /// orphan-pair rule). 2–3 is the range every surveyed project ships; 0 is legal (keep nothing).
    pub keep_recent_turns: usize,
    /// Cap on the summary itself, so the cure can't become the disease.
    pub summary_max_tokens: u32,
    /// Per-tool-result truncation in the transcript shown to the summarizer (OpenCode's 2k).
    pub tool_result_max_chars: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            trigger_tokens: 48_000,
            keep_recent_turns: 3,
            summary_max_tokens: 1_024,
            tool_result_max_chars: 2_000,
        }
    }
}

// chars/4 is the standard fast token proxy; ×1.3 because provider tokenizers undercount code and
// JSON payloads (Kilo Code's measured correction). Precision isn't the point — a consistent,
// slightly-conservative estimate is.
const CHARS_PER_TOKEN: f32 = 4.0;
const ESTIMATE_SAFETY_FACTOR: f32 = 1.3;

/// Estimated token count of a message list — contents plus tool-call JSON (which real tokenizers
/// count too). Tool *schemas* are excluded by design: they're constant per turn and absorbed by
/// the reserve built into [`CompactionConfig::trigger_tokens`].
pub fn estimate_tokens(messages: &[Message]) -> u32 {
    let chars: usize = messages.iter().map(message_chars).sum();
    (chars as f32 / CHARS_PER_TOKEN * ESTIMATE_SAFETY_FACTOR).ceil() as u32
}

fn message_chars(m: &Message) -> usize {
    let mut n = m.content.len();
    for call in &m.tool_calls {
        n += call.name.len() + call.arguments.to_string().len();
    }
    n += m.tool_call_id.as_deref().map_or(0, str::len);
    n
}

/// The index at which the kept tail begins, or `None` when there is nothing worth summarizing.
///
/// `history[0]` is the system root and is never elided; the elided region is `history[1..boundary]`
/// and must contain at least one message. The boundary is always a **user message index** (the
/// start of the K-th-from-last user turn), which is what guarantees an assistant `tool_calls`
/// message and its `tool` results — which always live inside one user-turn group — can never be
/// split across the summary/tail seam.
pub fn elision_boundary(history: &[Message], keep_recent_turns: usize) -> Option<usize> {
    let boundary = if keep_recent_turns == 0 {
        history.len()
    } else {
        let user_indices: Vec<usize> = history
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == Role::User)
            .map(|(i, _)| i)
            .collect();
        if user_indices.len() <= keep_recent_turns {
            return None;
        }
        user_indices[user_indices.len() - keep_recent_turns]
    };
    // `<= 1` covers both "the boundary is the root itself" and "the elided region would be empty"
    // (boundary == 1) — in both cases there is nothing to summarize.
    if boundary <= 1 {
        return None;
    }
    Some(boundary)
}

/// Render the elided region as a role-labeled plain-text transcript for the summarizer. Tool
/// results (and tool-call arguments) are truncated per `tool_result_max_chars` — the summary
/// needs to know a tool ran and what came of it, not re-read a 50KB file dump.
///
/// System-role messages render as `[system]`: the elided region never contains the root prompt,
/// but a *previous compaction marker* is system-role, and its summary must stay visible in the
/// transcript so the rolling summary carries forward.
pub fn render_transcript(messages: &[Message], tool_result_max_chars: usize) -> String {
    let mut out = String::new();
    for m in messages {
        match m.role {
            Role::System => {
                out.push_str("[system]\n");
                out.push_str(&m.content);
            }
            Role::User => {
                out.push_str("[user]\n");
                out.push_str(&m.content);
            }
            Role::Assistant => {
                out.push_str("[assistant]\n");
                out.push_str(&m.content);
                for call in &m.tool_calls {
                    out.push_str(&format!(
                        "\n(called tool `{}` with arguments: {})",
                        call.name,
                        truncate_chars(&call.arguments.to_string(), tool_result_max_chars)
                    ));
                }
            }
            Role::Tool => {
                out.push_str("[tool result]\n");
                out.push_str(&truncate_chars(&m.content, tool_result_max_chars));
            }
        }
        out.push_str("\n\n");
    }
    out
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("… [truncated]");
    out
}

/// The summarizer's system prompt — an "anchored context summarization assistant" (the prompt
/// shape Kilo Code and OpenCode converged on): a fixed Markdown skeleton, only facts present in
/// the transcript, and explicit rolling-summary semantics when the transcript opens with a
/// previous marker.
pub const SUMMARIZER_SYSTEM_PROMPT: &str = "\
You are an anchored context-summarization assistant. You compress the older part of an ongoing \
conversation into a structured rolling summary, so the conversation can continue within the \
model's context window without losing what still matters.

Rules:
- Use ONLY facts present in the transcript. Never invent, infer beyond it, or editorialize.
- If the transcript contains a previous summary (a [system] entry beginning \"[context compacted\"\
), carry its still-relevant content forward and update it — this is a ROLLING summary of the whole \
conversation so far, not a fresh summary of only the new part.
- Drop chit-chat, dead ends, superseded plans, and stale tool output. Keep what upcoming turns \
still need.
- Write in the same language the conversation is in.

Output EXACTLY this Markdown structure (omit a section only when it is genuinely empty):

## Goal
What the user is trying to accomplish overall.
## Constraints & preferences
Standing requirements, tastes, and hard limits the user stated.
## Progress
What is done, what is in progress, what is blocked.
## Key facts & decisions
Concrete facts, names, values, and choices made (with the why, when stated).
## Pending asks & next steps
Open questions to the user, promised follow-ups, the immediate next action.
## Relevant files, tools & artifacts
Paths, ids, tool names, and resources referenced that still matter.";

/// Build the one plain completion that produces the rolling summary. Temperature 0 (deterministic,
/// like the dispatcher) and a hard output cap from [`CompactionConfig::summary_max_tokens`].
pub fn summary_request(elided: &[Message], config: &CompactionConfig) -> CompletionRequest {
    let transcript = render_transcript(elided, config.tool_result_max_chars);
    CompletionRequest::new(vec![
        Message::system(SUMMARIZER_SYSTEM_PROMPT),
        Message::user(format!(
            "Summarize this conversation transcript:\n\n{transcript}"
        )),
    ])
    .with_temperature(0.0)
    .with_max_tokens(config.summary_max_tokens)
}

/// The marker node's message: system-role (it *is* context, not something anyone said), headed by
/// [`SUMMARY_HEADER`] so rendered history shows an honest checkpoint bubble and the next
/// summarizer recognizes it.
pub fn marker_message(summary: &str) -> Message {
    Message::system(format!("{SUMMARY_HEADER}\n\n{summary}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_provider::ToolInvocation;

    fn user(text: &str) -> Message {
        Message::user(text)
    }

    fn assistant(text: &str) -> Message {
        Message::assistant(text)
    }

    // [system, u1, a1, u2, a2, u3, a3] — the canonical linear chat shape.
    fn three_turn_history() -> Vec<Message> {
        vec![
            Message::system("sys"),
            user("u1"),
            assistant("a1"),
            user("u2"),
            assistant("a2"),
            user("u3"),
            assistant("a3"),
        ]
    }

    #[test]
    fn estimate_scales_with_content_and_counts_tool_calls() {
        let plain = estimate_tokens(&[assistant("abcd")]);
        let with_call = estimate_tokens(&[Message {
            role: Role::Assistant,
            content: "abcd".into(),
            tool_calls: vec![ToolInvocation::new(
                "t1",
                "some_tool",
                serde_json::json!({"a": 1}),
            )],
            tool_call_id: None,
        }]);
        assert!(with_call > plain, "tool-call JSON must add to the estimate");
        assert_eq!(plain, (4.0_f32 / 4.0 * 1.3).ceil() as u32);
    }

    #[test]
    fn boundary_keeps_last_k_user_turns() {
        let h = three_turn_history();
        // Keep 2 → tail starts at u2 (index 3); elided region is [u1, a1].
        assert_eq!(elision_boundary(&h, 2), Some(3));
        // Keep 1 → tail starts at u3 (index 5); elided region is [u1, a1, u2, a2].
        assert_eq!(elision_boundary(&h, 1), Some(5));
        // Keep 0 → everything but the root is summarized.
        assert_eq!(elision_boundary(&h, 0), Some(h.len()));
    }

    #[test]
    fn boundary_none_when_too_little_to_summarize() {
        let h = three_turn_history();
        // Keeping 3 of 3 user turns leaves nothing to elide.
        assert_eq!(elision_boundary(&h, 3), None);
        // A single-turn conversation never compacts, regardless of keep.
        let short = vec![Message::system("sys"), user("u1"), assistant("a1")];
        assert_eq!(elision_boundary(&short, 1), None);
    }

    #[test]
    fn boundary_never_splits_tool_call_pairs() {
        // assistant(tool_calls) + tool results live *inside* one user-turn group; a user-anchored
        // boundary can therefore never separate a call from its result.
        let h = vec![
            Message::system("sys"),
            user("u1"),
            Message {
                role: Role::Assistant,
                content: String::new(),
                tool_calls: vec![ToolInvocation::new("t1", "search", serde_json::json!({}))],
                tool_call_id: None,
            },
            Message::tool_result("t1", "results"),
            assistant("here you go"),
            user("u2"),
            assistant("a2"),
        ];
        let b = elision_boundary(&h, 1).unwrap();
        assert_eq!(b, 5, "boundary is the last user message");
        // The whole tool exchange falls in the elided region, call and result together.
        assert!(matches!(h[b - 1].role, Role::Assistant));
    }

    #[test]
    fn transcript_labels_roles_and_truncates_tool_results() {
        let long_result = "r".repeat(100);
        let h = vec![user("hello"), Message::tool_result("t1", &long_result)];
        let t = render_transcript(&h, 10);
        assert!(t.contains("[user]\nhello"));
        assert!(t.contains("[tool result]\n"));
        assert!(t.contains("… [truncated]"));
        assert!(
            !t.contains(&long_result),
            "full tool dump must not reach the summarizer"
        );
    }

    #[test]
    fn summary_request_is_deterministic_and_capped() {
        let config = CompactionConfig {
            summary_max_tokens: 77,
            ..CompactionConfig::default()
        };
        let req = summary_request(&three_turn_history()[1..3], &config);
        assert_eq!(req.temperature, Some(0.0));
        assert_eq!(req.max_tokens, Some(77));
        assert!(matches!(req.messages[0].role, Role::System));
        assert!(req.messages[1].content.contains("[user]\nu1"));
    }

    #[test]
    fn marker_message_carries_the_header() {
        let m = marker_message("## Goal\nstuff");
        assert!(matches!(m.role, Role::System));
        assert!(m.content.starts_with(SUMMARY_HEADER));
        assert!(m.content.contains("## Goal"));
    }
}
