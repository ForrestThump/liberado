//! Conversation-tree rollup: join child correlations via dispatch `parent_conversation`.

use std::collections::HashMap;

use crate::journal::JournalEvent;
use crate::price::{PriceTable, price_event};
use crate::report::Report;
use serde::{Deserialize, Serialize};

/// Per-root-conversation aggregates (face chat id, after child→parent join).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationRollup {
    pub conversation_id: String,
    pub calls: usize,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub cached_prompt_tokens: Option<u32>,
    /// Sum of priced calls only; `None` if every call was unpriced/unknown.
    pub cost_usd: Option<f64>,
    pub unpriced_calls: usize,
    pub total_repeat_calls: Option<usize>,
    /// Child correlations rolled into this conversation (for transparency).
    pub child_correlations: Vec<String>,
}

/// Per-role aggregates (after the same parent join — cost attributed where the work ran, under
/// the root conversation's tree, but role labels stay on the call).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleRollup {
    pub role: String,
    pub calls: usize,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub cost_usd: Option<f64>,
    pub unpriced_calls: usize,
}

/// Prompt-token growth for successive **agent turns** within one conversation.
///
/// # Turn boundary heuristic
///
/// The latency journal has no separate turn id. A face tool-calling loop records one
/// [`JournalEvent`](crate::JournalEvent) per model hop. Only the conversation's **own** hops —
/// those whose `correlation` is the root conversation itself — define turn boundaries:
///
/// - `tool_calls > 0` → the model asked for tools; the turn **continues**.
/// - `tool_calls == 0` → the model stopped; the turn **closes**.
/// - `finish == "error"` → the turn **closes** even if tool calls were requested (a turn can end by
///   failing).
///
/// **Child-dispatch calls are spend, not structure.** A delegated subagent runs its own multi-hop
/// loop, and its final hop reports `tool_calls == 0` like any other. Letting that close the *parent's*
/// turn splits one delegating turn in two and — worse — makes the row's `prompt_tokens` the
/// subagent's context while its `role` still says `face`. So a child hop is folded into whichever
/// turn is open for cost, and is never allowed to close one or to supply `prompt_tokens`/`role`.
/// A conversation made **only** of child correlations (no parent hop ever recorded) falls back to
/// treating every event as boundary-defining, so its spend is still grouped rather than collapsed
/// into a single unbounded turn.
///
/// This is a **heuristic**: it matches the executor's multi-hop loop on honest journals and is the
/// only boundary recoverable without a journal shape change.
///
/// # Fields
///
/// - `prompt_tokens` is the **last parent hop's** prompt size (end-of-turn context), so
///   `prompt_delta` is turn-over-turn growth, not hop-over-hop inside a multi-call turn and not
///   contaminated by a subagent's context.
/// - `role` and `correlation` come from the turn's first parent hop.
/// - `cost_usd` sums every priced hop in the turn, child hops included; unpriced hops inside a turn
///   never force `Some(0.0)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnGrowth {
    pub conversation_id: String,
    pub turn_index: usize,
    pub ts_ms: u64,
    pub correlation: String,
    pub role: String,
    pub prompt_tokens: Option<u32>,
    /// `prompt_tokens` delta vs previous **turn** in this conversation; `None` if either side absent.
    pub prompt_delta: Option<i64>,
    pub cost_usd: Option<f64>,
}

/// Whether this event ends a multi-hop agent turn (see [`TurnGrowth`]).
pub fn closes_turn(event: &JournalEvent) -> bool {
    event.tool_calls == 0 || event.finish == "error"
}

/// Tokens for a model the rates cannot price — never folded into a money total as 0.0. Covers both
/// "no entry at all" and "entry missing a rate this usage needs".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnpricedLine {
    pub model: String,
    pub calls: usize,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub cached_prompt_tokens: Option<u32>,
}

/// Resolve the root conversation id for an event's correlation.
pub fn root_conversation(correlation: &str, child_to_parent: &HashMap<String, String>) -> String {
    // Follow one hop (dispatch journals name the face chat). Guard against self-maps.
    match child_to_parent.get(correlation) {
        Some(parent) if parent != correlation => parent.clone(),
        _ => correlation.to_string(),
    }
}

/// Group events by root conversation (join children via dispatch parent map).
pub fn rollup_conversations(
    events: &[JournalEvent],
    child_to_parent: &HashMap<String, String>,
    prices: &PriceTable,
) -> Vec<ConversationRollup> {
    let mut by_root: HashMap<String, Acc> = HashMap::new();
    let mut children_seen: HashMap<String, Vec<String>> = HashMap::new();

    for event in events {
        let root = root_conversation(&event.correlation, child_to_parent);
        if event.correlation != root {
            children_seen
                .entry(root.clone())
                .or_default()
                .push(event.correlation.clone());
        }
        let priced = price_event(event, prices);
        let acc = by_root.entry(root).or_default();
        acc.add(&priced.event, priced.cost_usd, priced.cost_unknown);
    }

    let mut rows: Vec<ConversationRollup> = by_root
        .into_iter()
        .map(|(id, acc)| {
            let mut kids = children_seen.remove(&id).unwrap_or_default();
            kids.sort();
            kids.dedup();
            acc.into_conversation(id, kids)
        })
        .collect();
    rows.sort_by(|a, b| {
        b.cost_usd
            .partial_cmp(&a.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.conversation_id.cmp(&b.conversation_id))
    });
    rows
}

fn rollup_roles(events: &[JournalEvent], prices: &PriceTable) -> Vec<RoleRollup> {
    let mut by_role: HashMap<String, Acc> = HashMap::new();
    for event in events {
        let priced = price_event(event, prices);
        by_role.entry(event.role.clone()).or_default().add(
            &priced.event,
            priced.cost_usd,
            priced.cost_unknown,
        );
    }
    let mut rows: Vec<RoleRollup> = by_role
        .into_iter()
        .map(|(role, acc)| acc.into_role(role))
        .collect();
    rows.sort_by(|a, b| {
        b.cost_usd
            .partial_cmp(&a.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.role.cmp(&b.role))
    });
    rows
}

fn turn_growth(
    events: &[JournalEvent],
    child_to_parent: &HashMap<String, String>,
    prices: &PriceTable,
) -> Vec<TurnGrowth> {
    let mut indexed: Vec<(String, &JournalEvent)> = events
        .iter()
        .map(|e| (root_conversation(&e.correlation, child_to_parent), e))
        .collect();
    indexed.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.ts_ms.cmp(&b.1.ts_ms))
            .then(a.1.correlation.cmp(&b.1.correlation))
    });

    let mut out = Vec::new();
    let mut i = 0;
    while i < indexed.len() {
        let conv = indexed[i].0.clone();
        let conv_start = i;
        let conv_end = {
            let mut j = i;
            while j < indexed.len() && indexed[j].0 == conv {
                j += 1;
            }
            j
        };
        // Only the conversation's own hops define boundaries. A conversation consisting purely of
        // child correlations has none, so fall back to every event defining one — otherwise its
        // whole history would collapse into a single never-closing turn.
        let has_own_hops = indexed[conv_start..conv_end]
            .iter()
            .any(|(_, e)| e.correlation == conv);
        let defines_boundary = |e: &JournalEvent| !has_own_hops || e.correlation == conv;

        let mut turn_index = 0usize;
        let mut prev_prompt: Option<u32> = None;
        while i < conv_end {
            // Collect one multi-hop turn: open on the first hop, close on the first *parent* hop
            // with tool_calls==0 or finish=="error". Child hops are spend only.
            let first = indexed[i].1;
            let mut first_own: Option<&JournalEvent> = None;
            let mut last_own: Option<&JournalEvent> = None;
            let mut cost_usd: Option<f64> = None;
            loop {
                let e = indexed[i].1;
                let priced = price_event(e, prices);
                if let Some(c) = priced.cost_usd {
                    cost_usd = Some(cost_usd.unwrap_or(0.0) + c);
                }
                let boundary = defines_boundary(e);
                if boundary {
                    first_own.get_or_insert(e);
                    last_own = Some(e);
                }
                i += 1;
                if (boundary && closes_turn(e)) || i >= conv_end {
                    break;
                }
            }
            // `i` advanced past the last hop of this turn.
            let last_any = indexed[i - 1].1;
            // Context size is the parent's, never a subagent's; fall back only if this turn had no
            // parent hop at all (trailing child work after the turn already closed).
            let anchor = last_own.unwrap_or(last_any);
            let label = first_own.unwrap_or(first);
            let prompt = anchor.prompt_tokens;
            let prompt_delta = match (prev_prompt, prompt) {
                (Some(prev), Some(cur)) => Some(i64::from(cur) - i64::from(prev)),
                _ => None,
            };
            if prompt.is_some() {
                prev_prompt = prompt;
            }
            out.push(TurnGrowth {
                conversation_id: conv.clone(),
                turn_index,
                ts_ms: last_any.ts_ms,
                correlation: label.correlation.clone(),
                role: label.role.clone(),
                prompt_tokens: prompt,
                prompt_delta,
                cost_usd,
            });
            turn_index += 1;
        }
    }
    out
}

/// Put the turn table in the same order as the conversation table — most expensive first.
///
/// Growth was previously grouped by correlation id, which is alphabetical, so on the real journal
/// every row a reader saw belonged to the `"-"` bucket and 1,276 turns were truncated away. The
/// question this table exists to answer is "does a conversation's prompt keep growing", and it can
/// only answer it for the conversations someone would ask about.
fn order_turns_by_conversation_cost(
    turns: Vec<TurnGrowth>,
    conversations: &[ConversationRollup],
) -> Vec<TurnGrowth> {
    let rank: HashMap<&str, usize> = conversations
        .iter()
        .enumerate()
        .map(|(i, c)| (c.conversation_id.as_str(), i))
        .collect();
    let mut turns = turns;
    turns.sort_by(|a, b| {
        let ra = rank.get(a.conversation_id.as_str()).copied();
        let rb = rank.get(b.conversation_id.as_str()).copied();
        ra.cmp(&rb)
            .then_with(|| a.turn_index.cmp(&b.turn_index))
            .then_with(|| a.ts_ms.cmp(&b.ts_ms))
    });
    turns
}

fn unpriced_lines(events: &[JournalEvent], prices: &PriceTable) -> Vec<UnpricedLine> {
    let mut by_model: HashMap<String, Acc> = HashMap::new();
    for event in events {
        let priced = price_event(event, prices);
        if priced.cost_unknown {
            by_model
                .entry(event.model.clone())
                .or_default()
                .add(event, None, true);
        }
    }
    let mut rows: Vec<UnpricedLine> = by_model
        .into_iter()
        .map(|(model, acc)| UnpricedLine {
            model,
            calls: acc.calls,
            prompt_tokens: acc.prompt_tokens,
            completion_tokens: acc.completion_tokens,
            cached_prompt_tokens: acc.cached_prompt_tokens,
        })
        .collect();
    rows.sort_by(|a, b| a.model.cmp(&b.model));
    rows
}

/// Context occupancy of the most recent chat turn — the figure behind
/// `/api/status.token_usage_total`.
///
/// **Not** lifetime spend. Both consumers render this field *against* `context_window`: the TUI
/// status bar as `[N% ctx]` and `/status` as `Tokens: used / window (N% context)`. A running total
/// over the whole journal is a different quantity by orders of magnitude — on a journal with a few
/// thousand calls it pegs the gauge at the display cap permanently and says nothing about how full
/// the window actually is.
///
/// So: take the newest `face` event and report `prompt + completion` for that one call — the tokens
/// the model last had in front of it, which is what the next turn's context builds on. Non-face
/// roles are skipped because a subagent's context is not the chat's. No face event in the scanned
/// window → `None`, so the field stays absent rather than reporting a fabricated number.
///
/// [`total_tokens_from_events`] is the cumulative figure, and stays available for the cost report
/// where that *is* the question being asked.
pub fn context_tokens_from_events(events: &[JournalEvent]) -> Option<u64> {
    let last_face = events.iter().rev().find(|e| e.role == "face")?;
    match (last_face.prompt_tokens, last_face.completion_tokens) {
        (None, None) => None,
        (p, c) => Some(u64::from(p.unwrap_or(0)) + u64::from(c.unwrap_or(0))),
    }
}

/// Sum of usage the journal already records — every call, all history.
///
/// Prefer each event's `total_tokens` when present; otherwise `prompt + completion` when either is
/// present. Events with no usage reported contribute nothing (not zero). Empty / no-usage journal
/// → `None`.
///
/// This is *cumulative spend*, not context occupancy — see [`context_tokens_from_events`] for the
/// distinction and why `/api/status` needs the latter.
pub fn total_tokens_from_events(events: &[JournalEvent]) -> Option<u64> {
    let mut sum = 0u64;
    let mut any = false;
    for e in events {
        if let Some(t) = e.total_tokens {
            any = true;
            sum = sum.saturating_add(u64::from(t));
        } else if e.prompt_tokens.is_some() || e.completion_tokens.is_some() {
            any = true;
            sum = sum
                .saturating_add(u64::from(e.prompt_tokens.unwrap_or(0)))
                .saturating_add(u64::from(e.completion_tokens.unwrap_or(0)));
        }
    }
    if any { Some(sum) } else { None }
}

/// Cache hit rate = sum(cached_prompt_tokens) / sum(prompt_tokens) over events that reported
/// **both**. Absent fields are skipped (never coerced to zero).
pub fn cache_hit_rate(events: &[JournalEvent]) -> Option<f64> {
    let mut cached_sum: u64 = 0;
    let mut prompt_sum: u64 = 0;
    let mut any = false;
    for e in events {
        if let (Some(p), Some(c)) = (e.prompt_tokens, e.cached_prompt_tokens) {
            any = true;
            prompt_sum += u64::from(p);
            cached_sum += u64::from(c.min(p));
        }
    }
    if !any || prompt_sum == 0 {
        return if any { Some(0.0) } else { None };
    }
    Some(cached_sum as f64 / prompt_sum as f64)
}

/// Full report from events + parent map + rates.
pub fn build_report(
    events: &[JournalEvent],
    child_to_parent: &HashMap<String, String>,
    prices: &PriceTable,
) -> Report {
    let conversations = rollup_conversations(events, child_to_parent, prices);
    let roles = rollup_roles(events, prices);
    let turn_growth = order_turns_by_conversation_cost(
        turn_growth(events, child_to_parent, prices),
        &conversations,
    );
    let unpriced = unpriced_lines(events, prices);
    let cache_hit_rate = cache_hit_rate(events);

    let mut total: Option<f64> = None;
    let mut priced_calls = 0usize;
    let mut unpriced_calls = 0usize;
    let mut total_repeat_calls: Option<usize> = None;
    for e in events {
        let p = price_event(e, prices);
        if p.cost_unknown {
            unpriced_calls += 1;
        }
        if let Some(c) = p.cost_usd {
            priced_calls += 1;
            total = Some(total.unwrap_or(0.0) + c);
        }
        if let Some(r) = e.repeat_calls {
            total_repeat_calls = Some(total_repeat_calls.unwrap_or(0).saturating_add(r));
        }
    }

    Report {
        conversations,
        roles,
        turn_growth,
        unpriced,
        cache_hit_rate,
        total_cost_usd: total,
        event_count: events.len(),
        priced_calls,
        unpriced_calls,
        total_repeat_calls,
    }
}

#[derive(Default)]
struct Acc {
    calls: usize,
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
    cached_prompt_tokens: Option<u32>,
    cost_usd: Option<f64>,
    unpriced_calls: usize,
    repeat_calls: Option<usize>,
}

impl Acc {
    fn add(&mut self, event: &JournalEvent, cost: Option<f64>, cost_unknown: bool) {
        self.calls += 1;
        if cost_unknown {
            self.unpriced_calls += 1;
        }
        self.prompt_tokens = sum_opt(self.prompt_tokens, event.prompt_tokens);
        self.completion_tokens = sum_opt(self.completion_tokens, event.completion_tokens);
        self.cached_prompt_tokens = sum_opt(self.cached_prompt_tokens, event.cached_prompt_tokens);
        if let Some(c) = cost {
            self.cost_usd = Some(self.cost_usd.unwrap_or(0.0) + c);
        }
        if let Some(r) = event.repeat_calls {
            self.repeat_calls = Some(self.repeat_calls.unwrap_or(0).saturating_add(r));
        }
    }

    fn into_conversation(self, id: String, children: Vec<String>) -> ConversationRollup {
        ConversationRollup {
            conversation_id: id,
            calls: self.calls,
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            cached_prompt_tokens: self.cached_prompt_tokens,
            cost_usd: self.cost_usd,
            unpriced_calls: self.unpriced_calls,
            total_repeat_calls: self.repeat_calls,
            child_correlations: children,
        }
    }

    fn into_role(self, role: String) -> RoleRollup {
        RoleRollup {
            role,
            calls: self.calls,
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            cost_usd: self.cost_usd,
            unpriced_calls: self.unpriced_calls,
        }
    }
}

/// Sum optional counters: absent + absent = absent; present values add; absent does not become 0
/// when the other side is present (we only sum known contributions).
fn sum_opt(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (None, None) => None,
        (Some(x), None) | (None, Some(x)) => Some(x),
        (Some(x), Some(y)) => Some(x.saturating_add(y)),
    }
}
