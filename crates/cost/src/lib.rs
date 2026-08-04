//! Token cost accounting over the existing latency journal (D1 + D2).
//!
//! - **Prices** come from `[[models]]` optional per-million rates (`input` / `output` /
//!   `cached_input`) and are applied **at read time** — the journal never stores money.
//! - **Rollup** is a join, not a group-by: face records carry the chat id; subagent records carry a
//!   different correlation (`chat-delegate-<ulid>`); the link is the dispatch journal start record's
//!   `parent_conversation`.
//! - **Unpriced ≠ 0.0**: models without rates contribute tokens and appear on a separate unpriced
//!   line; they never pad a total with a silent zero.
//! - **Absent usage ≠ zero**: missing `prompt_tokens` / `cached_prompt_tokens` stay `None` end to
//!   end (streaming calls often report nothing).

mod journal;
mod price;
mod report;
mod rollup;

pub use journal::{
    JournalEvent, LoadError, child_to_parent_map, load_dispatch_parent_map, load_latency_events,
    load_latency_events_from_str,
};
pub use price::{PriceTable, PricedEvent, price_event, price_table_from_models};
pub use report::{Report, format_report};
pub use rollup::{
    ConversationRollup, RoleRollup, TurnGrowth, UnpricedLine, build_report, closes_turn,
    context_tokens_from_events, rollup_conversations, root_conversation, total_tokens_from_events,
};

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use liberado_common::{ModelProfile, ModelTokenPrices};
use liberado_config_loader::Topology;

/// Resolve the data dir: `LIBERADO_DATA_DIR` or `.liberado` under the current working directory.
pub fn default_data_dir() -> PathBuf {
    PathBuf::from(std::env::var("LIBERADO_DATA_DIR").unwrap_or_else(|_| ".liberado".into()))
}

/// Path to the latency events journal under a data dir.
pub fn latency_journal_path(data_dir: &Path) -> PathBuf {
    data_dir.join("latency").join("events.jsonl")
}

/// Path to the dispatches directory under a data dir.
pub fn dispatches_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("dispatches")
}

/// Load a price table from a topology TOML document (only `[[models]]` rates are used).
pub fn price_table_from_topology_toml(toml_src: &str) -> Result<PriceTable, String> {
    let topo: Topology =
        toml::from_str(toml_src).map_err(|e| format!("parse topology for prices: {e}"))?;
    Ok(price_table_from_models(&topo.models))
}

/// Load a price table from a topology.toml path.
pub fn price_table_from_topology_path(path: &Path) -> Result<PriceTable, String> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("read topology {}: {e}", path.display()))?;
    price_table_from_topology_toml(&src)
}

/// Build a full report from a data directory + price table.
pub fn report_from_data_dir(data_dir: &Path, prices: &PriceTable) -> Result<Report, LoadError> {
    let events = load_latency_events(&latency_journal_path(data_dir))?;
    let parents = load_dispatch_parent_map(&dispatches_dir(data_dir))?;
    Ok(build_report(&events, &parents, prices))
}

/// Context occupancy of the newest chat turn under `data_dir` — feeds
/// `/api/status.token_usage_total`, which its consumers render against `context_window`.
///
/// Reads only the journal's tail ([`journal::TAIL_SCAN_BYTES`]), because status is polled every few
/// seconds by every connected client and the journal grows without bound. Missing journal, no face
/// call in the window, or any read/parse failure → `None`: status must not fail, and must not
/// invent a number, over a journal it does not own.
pub fn context_tokens_for_data_dir(data_dir: &Path) -> Option<u64> {
    let events = journal::load_latency_events_tail(
        &latency_journal_path(data_dir),
        journal::TAIL_SCAN_BYTES,
    )?;
    context_tokens_from_events(&events)
}

/// Cumulative journaled token usage under `data_dir` — every call, all history.
///
/// Reads the whole journal, so it belongs to the CLI report rather than a polled endpoint. See
/// [`context_tokens_for_data_dir`] for the number `/api/status` needs.
pub fn token_usage_total_for_data_dir(data_dir: &Path) -> Option<u64> {
    match load_latency_events(&latency_journal_path(data_dir)) {
        Ok(events) => total_tokens_from_events(&events),
        Err(_) => None,
    }
}

/// Convenience for tests and pure callers: events + parent map + rates → report.
pub fn report_from_parts(
    events: &[JournalEvent],
    child_to_parent: &HashMap<String, String>,
    prices: &PriceTable,
) -> Report {
    build_report(events, child_to_parent, prices)
}

/// Helper to construct a prices table from (name, prices) pairs.
pub fn price_table_from_pairs<'a, I>(pairs: I) -> PriceTable
where
    I: IntoIterator<Item = (&'a str, ModelTokenPrices)>,
{
    let models: Vec<ModelProfile> = pairs
        .into_iter()
        .map(|(name, prices)| ModelProfile {
            name: name.into(),
            tool_calling: true,
            structured_output: false,
            context_window: 64_000,
            tier: liberado_common::ModelTier::WorkPlane,
            cost: None,
            prices,
        })
        .collect();
    price_table_from_models(&models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_common::ModelTokenPrices;

    fn event(
        correlation: &str,
        role: &str,
        model: &str,
        prompt: Option<u32>,
        completion: Option<u32>,
        cached: Option<u32>,
        ts_ms: u64,
    ) -> JournalEvent {
        event_full(
            correlation,
            role,
            model,
            prompt,
            completion,
            cached,
            ts_ms,
            0,
            "stop",
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn event_full(
        correlation: &str,
        role: &str,
        model: &str,
        prompt: Option<u32>,
        completion: Option<u32>,
        cached: Option<u32>,
        ts_ms: u64,
        tool_calls: usize,
        finish: &str,
    ) -> JournalEvent {
        JournalEvent {
            ts_ms,
            correlation: correlation.into(),
            role: role.into(),
            model: model.into(),
            kind: "llm_call".into(),
            wall_ms: 100,
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: match (prompt, completion) {
                (Some(p), Some(c)) => Some(p + c),
                (Some(p), None) => Some(p),
                (None, Some(c)) => Some(c),
                (None, None) => None,
            },
            cached_prompt_tokens: cached,
            finish: finish.into(),
            tool_calls,
            streamed: false,
            repeat_calls: None,
        }
    }

    fn rates(input: f64, output: f64, cached_input: f64) -> ModelTokenPrices {
        ModelTokenPrices {
            input: Some(input),
            output: Some(output),
            cached_input: Some(cached_input),
        }
    }

    /// Unpriced model: tokens present, cost is None, never contributes 0.0 to totals.
    #[test]
    fn unpriced_model_never_contributes_zero_to_total() {
        let events = vec![
            event(
                "conv-a",
                "face",
                "priced-model",
                Some(1_000_000),
                Some(0),
                None,
                1,
            ),
            event(
                "conv-a",
                "face",
                "mystery-model",
                Some(5_000_000),
                Some(1_000_000),
                None,
                2,
            ),
        ];
        let prices = price_table_from_pairs([("priced-model", rates(1.0, 2.0, 0.1))]);
        let report = report_from_parts(&events, &HashMap::new(), &prices);

        // Only the priced call: 1M input @ $1/M = $1.00
        assert!(
            (report.total_cost_usd.unwrap() - 1.0).abs() < 1e-9,
            "total must be 1.0 from priced model only, got {:?}",
            report.total_cost_usd
        );
        assert!(
            report.unpriced.iter().any(|u| u.model == "mystery-model"),
            "unpriced line must list mystery-model: {:?}",
            report.unpriced
        );
        let unpriced = report
            .unpriced
            .iter()
            .find(|u| u.model == "mystery-model")
            .unwrap();
        assert_eq!(unpriced.prompt_tokens, Some(5_000_000));
        assert_eq!(unpriced.completion_tokens, Some(1_000_000));
        // Explicit: the total is not (1.0 + 0.0) with a silent zero for mystery.
        assert_ne!(report.total_cost_usd, Some(0.0));
    }

    /// Face corr A + child corr B + dispatch start with parent_conversation=A → parent includes child.
    #[test]
    fn parent_rollup_includes_child_via_dispatch_parent_conversation() {
        let face = "chat-face-aaa";
        let child = "chat-delegate-bbb"; // deliberately different
        assert_ne!(face, child);

        let events = vec![
            event(face, "face", "m", Some(1000), Some(100), None, 10),
            event(child, "orchestrator", "m", Some(9000), Some(500), None, 20),
            event(child, "orchestrator", "m", Some(2000), Some(50), None, 30),
        ];
        let mut parents = HashMap::new();
        parents.insert(child.into(), face.into());

        let prices = price_table_from_pairs([("m", rates(1.0, 1.0, 1.0))]);
        let report = report_from_parts(&events, &parents, &prices);

        let conv = report
            .conversations
            .iter()
            .find(|c| c.conversation_id == face)
            .expect("parent conversation row");
        // All three calls roll into parent: prompt 1000+9000+2000=12000, completion 100+500+50=650
        // cost = (12000 + 650) / 1e6 * $1 = 0.01265
        assert_eq!(conv.prompt_tokens, Some(12_000));
        assert_eq!(conv.completion_tokens, Some(650));
        assert!((conv.cost_usd.unwrap() - 0.01265).abs() < 1e-9);

        // Child must not appear as its own billed row in the conversation table.
        assert!(
            !report
                .conversations
                .iter()
                .any(|c| c.conversation_id == child),
            "child correlation must not be a separate conversation row (would double-count)"
        );
    }

    #[test]
    fn repeat_calls_rolls_up_and_appears_in_report() {
        let prices = price_table_from_pairs([("m", rates(1.0, 2.0, 0.1))]);
        let events = vec![
            JournalEvent {
                repeat_calls: Some(3),
                ..event("conv", "face", "m", Some(100), Some(10), None, 1)
            },
            JournalEvent {
                repeat_calls: Some(1),
                ..event("conv", "orchestrator", "m", Some(200), Some(20), None, 2)
            },
            JournalEvent {
                repeat_calls: None,
                ..event("conv", "orchestrator", "m", Some(300), Some(30), None, 3)
            },
        ];
        let report = report_from_parts(&events, &HashMap::new(), &prices);

        assert_eq!(report.total_repeat_calls, Some(4));
        assert_eq!(report.conversations[0].total_repeat_calls, Some(4));
        assert!(
            format_report(&report).contains("repeat_calls: 4"),
            "report header must show repeat_calls"
        );
    }

    #[test]
    fn report_serializes_to_json_and_round_trips() {
        let events = vec![event("conv-a", "face", "m", Some(1000), Some(100), None, 1)];
        let prices = price_table_from_pairs([("m", rates(1.0, 2.0, 0.1))]);
        let report = report_from_parts(&events, &HashMap::new(), &prices);

        let json = serde_json::to_string_pretty(&report).expect("serialize");
        assert!(json.contains("conv-a"));
        assert!(json.contains("total_cost_usd"));

        let round_tripped: Report = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(round_tripped.event_count, report.event_count);
        assert_eq!(round_tripped.priced_calls, report.priced_calls);
    }

    /// Summing parent rows does not double-count child money; querying child scope alone is direct-only.
    #[test]
    fn no_double_count_when_child_also_queried() {
        let face = "conv-parent";
        let child = "dispatch-child";
        let events = vec![
            event(face, "face", "m", Some(1_000_000), Some(0), None, 1),
            event(
                child,
                "orchestrator",
                "m",
                Some(1_000_000),
                Some(0),
                None,
                2,
            ),
        ];
        let mut parents = HashMap::new();
        parents.insert(child.into(), face.into());
        let prices = price_table_from_pairs([("m", rates(1.0, 1.0, 1.0))]);

        let report = report_from_parts(&events, &parents, &prices);
        // Parent total $2 (both calls); only one conversation row.
        assert_eq!(report.conversations.len(), 1);
        assert!((report.total_cost_usd.unwrap() - 2.0).abs() < 1e-9);

        // Direct scope of the child correlation (events only under that corr, no re-parenting)
        // is a separate view for debugging — it is NOT added on top of parent in the main table.
        let child_direct: f64 = events
            .iter()
            .filter(|e| e.correlation == child)
            .filter_map(|e| price_event(e, &prices).cost_usd)
            .sum();
        assert!((child_direct - 1.0).abs() < 1e-9);

        // Parent row money already includes child_direct; main table total is not parent+child.
        let parent_row = &report.conversations[0];
        assert!((parent_row.cost_usd.unwrap() - 2.0).abs() < 1e-9);
        assert!((report.total_cost_usd.unwrap() - parent_row.cost_usd.unwrap()).abs() < 1e-9);
    }

    /// Changing rates on the same fixture changes the money total (read-time pricing).
    #[test]
    fn rate_change_on_same_fixture_changes_total() {
        let events = vec![event(
            "c1",
            "face",
            "m",
            Some(1_000_000),
            Some(1_000_000),
            None,
            1,
        )];
        let low = price_table_from_pairs([("m", rates(1.0, 1.0, 0.1))]);
        let high = price_table_from_pairs([("m", rates(10.0, 10.0, 1.0))]);

        let r_low = report_from_parts(&events, &HashMap::new(), &low);
        let r_high = report_from_parts(&events, &HashMap::new(), &high);

        assert!((r_low.total_cost_usd.unwrap() - 2.0).abs() < 1e-9);
        assert!((r_high.total_cost_usd.unwrap() - 20.0).abs() < 1e-9);
        assert_ne!(r_low.total_cost_usd, r_high.total_cost_usd);
    }

    /// Absent token fields stay distinguishable from zero (cache hit rate and cost).
    #[test]
    fn absent_usage_is_not_zero() {
        let with_usage = event("c", "face", "m", Some(100), Some(10), Some(40), 1);
        let no_usage = event("c", "face", "m", None, None, None, 2);
        let zero_usage = event("c", "face", "m", Some(0), Some(0), Some(0), 3);

        assert!(with_usage.prompt_tokens.is_some());
        assert!(no_usage.prompt_tokens.is_none());
        assert_eq!(zero_usage.prompt_tokens, Some(0));
        assert_ne!(no_usage.prompt_tokens, zero_usage.prompt_tokens);

        let prices = price_table_from_pairs([("m", rates(1.0, 1.0, 0.1))]);
        let priced_absent = price_event(&no_usage, &prices);
        let priced_zero = price_event(&zero_usage, &prices);
        // No usage → cost unknown (None), not $0.
        assert!(priced_absent.cost_usd.is_none());
        // Explicit zeros → $0.0 is meaningful.
        assert_eq!(priced_zero.cost_usd, Some(0.0));

        let report = report_from_parts(
            &[with_usage, no_usage, zero_usage],
            &HashMap::new(),
            &prices,
        );
        // Cache hit rate uses only events that reported both prompt and cached.
        // with_usage: 40/100 = 0.4; zero_usage: 0/0 is undefined and skipped; no_usage skipped.
        assert!((report.cache_hit_rate.unwrap() - 0.4).abs() < 1e-9);
        // Prompt tokens sum only known: 100 + 0 = 100 (absent not coerced to 0 in the sense that
        // cache math ignored it; sum for display may include only present — see rollup).
        assert_eq!(report.conversations[0].prompt_tokens, Some(100));
    }

    /// A model priced on one side only cannot price a call that uses the other. It must land on
    /// the unpriced line with its tokens, not contribute a partial (and therefore understated)
    /// figure to the money total.
    #[test]
    fn partially_priced_model_is_unpriceable_not_half_priced() {
        let half = ModelTokenPrices {
            input: Some(1.0),
            output: None,
            cached_input: None,
        };
        let prices = price_table_from_pairs([("half-priced", half)]);
        let e = event(
            "c",
            "face",
            "half-priced",
            Some(1_000_000),
            Some(1_000_000),
            None,
            1,
        );

        let priced = price_event(&e, &prices);
        assert!(
            priced.cost_usd.is_none(),
            "must not report $1.00 by pricing input and silently zeroing output"
        );
        assert!(
            priced.cost_unknown,
            "a rate the usage needs is missing — that is a pricing gap, not reported usage"
        );

        let report = report_from_parts(&[e], &HashMap::new(), &prices);
        assert_eq!(report.total_cost_usd, None);
        assert_eq!(report.unpriced_calls, 1);
        let line = report
            .unpriced
            .iter()
            .find(|u| u.model == "half-priced")
            .expect("tokens still reported for a model we cannot price");
        assert_eq!(line.completion_tokens, Some(1_000_000));
    }

    /// A call the provider reported no usage for is not a pricing gap. Both leave cost unknown,
    /// but only one is an operator action ("add a rate"), so they must stay distinguishable.
    #[test]
    fn absent_usage_is_not_counted_as_unpriced() {
        let prices = price_table_from_pairs([("m", rates(1.0, 2.0, 0.1))]);
        let no_usage = event("c", "face", "m", None, None, None, 1);

        let priced = price_event(&no_usage, &prices);
        assert!(priced.cost_usd.is_none());
        assert!(
            !priced.cost_unknown,
            "the model is priced; the backend simply said nothing about usage"
        );

        let report = report_from_parts(&[no_usage], &HashMap::new(), &prices);
        assert_eq!(report.unpriced_calls, 0);
        assert!(
            report.unpriced.is_empty(),
            "a priced model must not appear on the unpriced line: {:?}",
            report.unpriced
        );
    }

    /// Provider calls made outside a `with_correlation` scope share the id `"-"`. They are real
    /// spend, but they are not a conversation, and the table must not imply otherwise.
    #[test]
    fn uncorrelated_calls_are_labelled_not_shown_as_a_conversation() {
        let prices = price_table_from_pairs([("m", rates(1.0, 1.0, 1.0))]);
        let events = vec![event("-", "unknown", "m", Some(1000), Some(10), None, 1)];
        let report = report_from_parts(&events, &HashMap::new(), &prices);

        assert_eq!(report.conversations[0].conversation_id, "-");
        let text = format_report(&report);
        assert!(
            text.contains("(unattributed)"),
            "the '-' bucket must be named in the table: {text}"
        );
    }

    /// A face turn with three tool-calling hops is **one** turn row, not three.
    ///
    /// R1: if grouping is deleted (one row per event again), this fails — the fixture is multi-hop
    /// on purpose; a fixture where every call already has `tool_calls == 0` would pass either
    /// implementation and prove nothing.
    #[test]
    fn multi_hop_face_turn_is_one_turn_row() {
        let prices = price_table_from_pairs([("m", rates(1.0, 2.0, 0.1))]);
        let conv = "01KZMULTI000000000000000000";
        // Three hops: two with tool_calls > 0, last closes with tool_calls == 0.
        let events = vec![
            event_full(
                conv,
                "face",
                "m",
                Some(1_000),
                Some(50),
                None,
                1,
                2,
                "tool_calls",
            ),
            event_full(
                conv,
                "face",
                "m",
                Some(1_200),
                Some(40),
                None,
                2,
                1,
                "tool_calls",
            ),
            event_full(conv, "face", "m", Some(1_500), Some(80), None, 3, 0, "stop"),
        ];
        let report = report_from_parts(&events, &HashMap::new(), &prices);
        let turns: Vec<_> = report
            .turn_growth
            .iter()
            .filter(|t| t.conversation_id == conv)
            .collect();
        assert_eq!(
            turns.len(),
            1,
            "three hops must collapse to one turn; got {turns:?}"
        );
        assert_eq!(turns[0].turn_index, 0);
        assert_eq!(
            turns[0].prompt_tokens,
            Some(1_500),
            "row uses last hop's prompt (end-of-turn context)"
        );
        // Cost is the sum of all three hops, not the last alone.
        let expected: f64 = events
            .iter()
            .map(|e| price_event(e, &prices).cost_usd.unwrap())
            .sum();
        assert!(
            (turns[0].cost_usd.unwrap() - expected).abs() < 1e-9,
            "turn cost must sum hops: got {:?} expected {expected}",
            turns[0].cost_usd
        );
    }

    /// Per-turn prompt_delta is turn-over-turn: hops inside turn 1 do not create deltas, and
    /// turn 2's delta is against turn 1's final prompt — not against the middle hop.
    #[test]
    fn prompt_delta_is_turn_over_turn_not_hop_over_hop() {
        let prices = price_table_from_pairs([("m", rates(1.0, 1.0, 1.0))]);
        let conv = "01KZDELTA000000000000000000";
        let events = vec![
            // Turn 1: multi-hop — intra-hop prompts 1000 → 1100 → 1200
            event_full(
                conv,
                "face",
                "m",
                Some(1_000),
                Some(10),
                None,
                1,
                1,
                "tool_calls",
            ),
            event_full(
                conv,
                "face",
                "m",
                Some(1_100),
                Some(10),
                None,
                2,
                1,
                "tool_calls",
            ),
            event_full(conv, "face", "m", Some(1_200), Some(10), None, 3, 0, "stop"),
            // Turn 2: single hop at 1500 → delta should be +300 vs 1200, not vs 1100 or 1000
            event_full(conv, "face", "m", Some(1_500), Some(10), None, 4, 0, "stop"),
        ];
        let report = report_from_parts(&events, &HashMap::new(), &prices);
        let turns: Vec<_> = report
            .turn_growth
            .iter()
            .filter(|t| t.conversation_id == conv)
            .collect();
        assert_eq!(turns.len(), 2, "expected two turns, got {turns:?}");
        assert_eq!(turns[0].prompt_tokens, Some(1_200));
        assert_eq!(turns[0].prompt_delta, None, "first turn has no prior");
        assert_eq!(turns[1].prompt_tokens, Some(1_500));
        assert_eq!(
            turns[1].prompt_delta,
            Some(300),
            "delta must ignore intra-turn hops (1500-1200)"
        );
    }

    /// A hop that finishes with `finish == "error"` closes the turn even when more hops follow.
    #[test]
    fn error_finish_closes_a_turn() {
        let prices = price_table_from_pairs([("m", rates(1.0, 1.0, 1.0))]);
        let conv = "01KZERROR000000000000000000";
        let events = vec![
            event_full(
                conv,
                "face",
                "m",
                Some(500),
                Some(10),
                None,
                1,
                2,
                "tool_calls",
            ),
            // Closes despite tool_calls > 0 because finish is error.
            event_full(conv, "face", "m", Some(600), Some(0), None, 2, 1, "error"),
            event_full(conv, "face", "m", Some(700), Some(20), None, 3, 0, "stop"),
        ];
        let report = report_from_parts(&events, &HashMap::new(), &prices);
        let turns: Vec<_> = report
            .turn_growth
            .iter()
            .filter(|t| t.conversation_id == conv)
            .collect();
        assert_eq!(
            turns.len(),
            2,
            "error must close turn 0; next hop is turn 1: {turns:?}"
        );
        assert_eq!(turns[0].prompt_tokens, Some(600));
        assert_eq!(turns[1].prompt_tokens, Some(700));
        assert_eq!(turns[1].prompt_delta, Some(100));
    }

    /// A delegating face turn is **one** turn, and its numbers are the face's.
    ///
    /// R3: this crosses the child→parent join, which the other turn tests never touch — they are
    /// all single-correlation. A subagent's last hop reports `tool_calls == 0` exactly like a face
    /// hop, so without the parent-hop boundary rule it closes the *parent's* turn: the row's
    /// `prompt_tokens` becomes the subagent's context while `role` still reads `face`, and the next
    /// face hop is reported as a separate turn with a large negative delta.
    ///
    /// R1: two independent guards, verified separately. Dropping the `boundary &&` on the close
    /// check alone fails this at 2 turns (prompts 1000 / 1400). Also taking `anchor = last_any`
    /// instead of `last_own` reproduces the pre-fix shape exactly: 2 turns, prompt 9000,
    /// delta -7600 — a face row carrying the subagent's context.
    #[test]
    fn delegated_child_hops_do_not_split_or_contaminate_the_parent_turn() {
        let prices = price_table_from_pairs([("m", rates(1.0, 2.0, 0.1))]);
        let conv = "01KZDELEGATE0000000000000000";
        let child = "chat-delegate-01child000000000";
        let mut parents = HashMap::new();
        parents.insert(child.to_string(), conv.to_string());
        // One face turn: face asks for `delegate`, the subagent loops, the face replies.
        let events = vec![
            event_full(
                conv,
                "face",
                "m",
                Some(1_000),
                Some(10),
                None,
                1,
                1,
                "tool_calls",
            ),
            event_full(
                child,
                "orchestrator",
                "m",
                Some(5_000),
                Some(10),
                None,
                2,
                1,
                "tool_calls",
            ),
            event_full(
                child,
                "orchestrator",
                "m",
                Some(9_000),
                Some(10),
                None,
                3,
                0,
                "stop",
            ),
            event_full(conv, "face", "m", Some(1_400), Some(10), None, 4, 0, "stop"),
        ];
        let report = report_from_parts(&events, &parents, &prices);
        let turns: Vec<_> = report
            .turn_growth
            .iter()
            .filter(|t| t.conversation_id == conv)
            .collect();
        assert_eq!(
            turns.len(),
            1,
            "a subagent's closing hop must not split the face turn: {turns:?}"
        );
        assert_eq!(
            turns[0].prompt_tokens,
            Some(1_400),
            "context size must be the face's last hop, not the subagent's 9000"
        );
        assert_eq!(turns[0].role, "face");
        // The subagent's spend still belongs to the turn even though it shapes nothing.
        let expected: f64 = events
            .iter()
            .map(|e| price_event(e, &prices).cost_usd.unwrap())
            .sum();
        assert!(
            (turns[0].cost_usd.unwrap() - expected).abs() < 1e-9,
            "turn cost must include child hops: got {:?} expected {expected}",
            turns[0].cost_usd
        );
    }

    /// A conversation that is *only* child correlations still gets grouped turns rather than one
    /// unbounded turn — the fallback when no parent hop was ever recorded (the shape the round-1
    /// `-` bucket had before correlation coverage landed).
    #[test]
    fn child_only_conversation_falls_back_to_per_hop_boundaries() {
        let prices = price_table_from_pairs([("m", rates(1.0, 2.0, 0.1))]);
        let conv = "01KZORPHAN000000000000000000";
        let child = "chat-delegate-01orphan00000000";
        let mut parents = HashMap::new();
        parents.insert(child.to_string(), conv.to_string());
        let events = vec![
            event_full(
                child,
                "orchestrator",
                "m",
                Some(100),
                Some(10),
                None,
                1,
                0,
                "stop",
            ),
            event_full(
                child,
                "orchestrator",
                "m",
                Some(300),
                Some(10),
                None,
                2,
                0,
                "stop",
            ),
        ];
        let report = report_from_parts(&events, &parents, &prices);
        let turns: Vec<_> = report
            .turn_growth
            .iter()
            .filter(|t| t.conversation_id == conv)
            .collect();
        assert_eq!(turns.len(), 2, "no parent hop → every hop is a boundary");
        assert_eq!(turns[1].prompt_delta, Some(200));
    }

    /// An unpriced hop inside a multi-hop turn does not force the turn's cost to `Some(0.0)`.
    #[test]
    fn unpriced_hop_inside_turn_does_not_zero_turn_cost() {
        let prices = price_table_from_pairs([("priced", rates(1.0, 2.0, 0.1))]);
        let conv = "01KZUNPRICEDTURN00000000000";
        let events = vec![
            event_full(
                conv,
                "face",
                "priced",
                Some(1_000_000),
                Some(0),
                None,
                1,
                1,
                "tool_calls",
            ),
            // Unpriced model mid-turn
            event_full(
                conv,
                "face",
                "mystery",
                Some(50_000),
                Some(10),
                None,
                2,
                0,
                "stop",
            ),
        ];
        let report = report_from_parts(&events, &HashMap::new(), &prices);
        let turns: Vec<_> = report
            .turn_growth
            .iter()
            .filter(|t| t.conversation_id == conv)
            .collect();
        assert_eq!(turns.len(), 1);
        // First hop alone: 1M prompt @ $1/M = $1.00
        assert!(
            turns[0].cost_usd.is_some(),
            "priced hop must contribute money"
        );
        assert!(
            (turns[0].cost_usd.unwrap() - 1.0).abs() < 1e-9,
            "cost must be the priced hop only, not 0.0: {:?}",
            turns[0].cost_usd
        );
        assert_ne!(
            turns[0].cost_usd,
            Some(0.0),
            "unpriced mid-turn must never collapse cost to 0.0"
        );
    }

    /// Context occupancy is the newest **face** call's prompt+completion — not a running total,
    /// and not a subagent's context even when the subagent called more recently.
    #[test]
    fn context_tokens_is_newest_face_call_not_a_running_total() {
        let events = vec![
            event("c", "face", "m", Some(200), Some(30), None, 1),
            event("c", "face", "m", Some(40), Some(10), None, 2),
            // Newer, much larger, and not the chat's context.
            event("kid", "orchestrator", "m", Some(90_000), Some(5), None, 3),
        ];
        assert_eq!(context_tokens_from_events(&events), Some(50));
        // The cumulative reading of the same fixture is a different number entirely.
        assert_eq!(total_tokens_from_events(&events), Some(90_285));
        // No face call at all → absent, never a fabricated 0.
        assert_eq!(
            context_tokens_from_events(&[event(
                "kid",
                "orchestrator",
                "m",
                Some(7),
                Some(1),
                None,
                1
            )]),
            None
        );
        assert_eq!(context_tokens_from_events(&[]), None);
        // Face call that reported no usage → absent.
        assert_eq!(
            context_tokens_from_events(&[event("c", "face", "m", None, None, None, 1)]),
            None
        );
    }

    /// The tail reader returns only complete records from the end of the file, and drops the
    /// partial first line when the window starts mid-record. This is what keeps `/api/status`
    /// bounded on a journal that grows forever.
    #[test]
    fn journal_tail_drops_partial_head_and_keeps_complete_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let line = |ts: u64, prompt: u32| {
            format!(
                r#"{{"ts_ms":{ts},"correlation":"c","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":{prompt},"completion_tokens":1,"finish":"stop","tool_calls":0,"streamed":false}}"#
            )
        };
        let body = format!("{}\n{}\n{}\n", line(1, 10), line(2, 20), line(3, 30));
        std::fs::write(&path, &body).unwrap();

        // Whole file fits: every record survives.
        let all = journal::load_latency_events_tail(&path, 1024 * 1024).unwrap();
        assert_eq!(all.len(), 3);

        // Window smaller than the file: starts mid-record, so the partial head is dropped and only
        // whole records come back — crucially, never a parse error.
        let one_line_len = body.lines().next().unwrap().len() as u64 + 1;
        let tail = journal::load_latency_events_tail(&path, one_line_len + 5).unwrap();
        assert_eq!(tail.len(), 1, "got {tail:?}");
        assert_eq!(tail[0].ts_ms, 3, "the tail must be the newest record");
        assert_eq!(context_tokens_from_events(&tail), Some(31));

        // Missing file → None, not a panic.
        assert!(journal::load_latency_events_tail(&dir.path().join("nope.jsonl"), 1024).is_none());
    }

    #[test]
    fn total_tokens_from_events_sums_reported_usage() {
        let events = vec![
            event("a", "face", "m", Some(100), Some(20), None, 1),
            event("b", "face", "m", Some(50), None, None, 2),
            // no usage — skipped
            event("c", "face", "m", None, None, None, 3),
        ];
        assert_eq!(total_tokens_from_events(&events), Some(170));
        assert_eq!(total_tokens_from_events(&[]), None);
        assert_eq!(
            total_tokens_from_events(&[event("c", "face", "m", None, None, None, 1)]),
            None
        );
    }

    #[test]
    fn token_usage_total_for_data_dir_reads_journal() {
        let dir = tempfile::tempdir().unwrap();
        let latency = dir.path().join("latency");
        std::fs::create_dir_all(&latency).unwrap();
        std::fs::write(
            latency.join("events.jsonl"),
            r#"{"ts_ms":1,"correlation":"c","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":100,"completion_tokens":25,"total_tokens":125,"finish":"stop","tool_calls":0,"streamed":false}
{"ts_ms":2,"correlation":"c","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":50,"completion_tokens":10,"total_tokens":60,"finish":"stop","tool_calls":0,"streamed":false}
"#,
        )
        .unwrap();
        assert_eq!(token_usage_total_for_data_dir(dir.path()), Some(185));
        assert_eq!(
            token_usage_total_for_data_dir(dir.path().join("missing").as_path()),
            None
        );
    }

    /// The turn table is printed truncated, so its order decides what a reader ever sees. It must
    /// lead with the conversations the cost table leads with, not with whichever id sorts first.
    #[test]
    fn turn_growth_leads_with_the_expensive_conversation() {
        let prices = price_table_from_pairs([("m", rates(1.0, 1.0, 1.0))]);
        // "-" (the uncorrelated bucket) sorts before any ULID but is the cheaper of the two.
        let events = vec![
            event("-", "face", "m", Some(1_000), Some(10), None, 1),
            event("-", "face", "m", Some(1_200), Some(10), None, 2),
            event(
                "01KZEXPENSIVE00000000000000",
                "face",
                "m",
                Some(500_000),
                Some(100),
                None,
                3,
            ),
            event(
                "01KZEXPENSIVE00000000000000",
                "face",
                "m",
                Some(700_000),
                Some(100),
                None,
                4,
            ),
        ];
        let report = report_from_parts(&events, &HashMap::new(), &prices);

        assert_eq!(
            report.conversations[0].conversation_id, "01KZEXPENSIVE00000000000000",
            "precondition: the ULID conversation is the expensive one"
        );
        assert_eq!(
            report.turn_growth[0].conversation_id, "01KZEXPENSIVE00000000000000",
            "turn growth must follow the cost table's order, not id order"
        );
        // And within a conversation, turns stay in the order they happened.
        assert_eq!(report.turn_growth[0].prompt_tokens, Some(500_000));
        assert_eq!(report.turn_growth[1].prompt_delta, Some(200_000));
    }

    /// Cached tokens use the cached_input rate, not the full input rate.
    #[test]
    fn cached_input_priced_cheaper() {
        // 1M prompt of which 500k cached, 0 completion.
        // uncached 500k @ $1/M + cached 500k @ $0.1/M = 0.5 + 0.05 = 0.55
        let e = event("c", "face", "m", Some(1_000_000), Some(0), Some(500_000), 1);
        let prices = price_table_from_pairs([("m", rates(1.0, 2.0, 0.1))]);
        let p = price_event(&e, &prices);
        assert!((p.cost_usd.unwrap() - 0.55).abs() < 1e-9);
    }

    /// Topology TOML flat keys feed the price table.
    #[test]
    fn prices_load_from_topology_toml() {
        let toml = r#"
            vault_path = "/tmp/v"
            [[models]]
            name = "deepseek/deepseek-v4-flash"
            tool_calling = true
            structured_output = true
            context_window = 128000
            tier = "control_plane"
            input = 0.14
            output = 0.28
            cached_input = 0.014
        "#;
        let table = price_table_from_topology_toml(toml).unwrap();
        let r = table.get("deepseek/deepseek-v4-flash").unwrap();
        assert_eq!(r.input, Some(0.14));
        assert_eq!(r.output, Some(0.28));
        assert_eq!(r.cached_input, Some(0.014));
    }

    /// End-to-end against a temp data dir: events.jsonl + dispatch journal + prices.
    #[test]
    fn fixture_data_dir_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        let latency = data.join("latency");
        let dispatches = data.join("dispatches");
        std::fs::create_dir_all(&latency).unwrap();
        std::fs::create_dir_all(&dispatches).unwrap();

        let face = "01faceconv0000000000000000";
        let child = "chat-delegate-01child000000000";

        let events = format!(
            r#"{{"ts_ms":1,"correlation":"{face}","role":"face","model":"m","kind":"llm_call","wall_ms":10,"prompt_tokens":1000,"completion_tokens":100,"finish":"stop","tool_calls":0,"streamed":false}}
{{"ts_ms":2,"correlation":"{child}","role":"orchestrator","model":"m","kind":"llm_call","wall_ms":20,"prompt_tokens":5000,"completion_tokens":200,"cached_prompt_tokens":4000,"finish":"stop","tool_calls":1,"streamed":true}}
{{"ts_ms":3,"correlation":"{face}","role":"face","model":"unpriced","kind":"llm_call","wall_ms":5,"prompt_tokens":50,"completion_tokens":5,"finish":"stop","tool_calls":0,"streamed":false}}
"#
        );
        std::fs::write(latency.join("events.jsonl"), events).unwrap();

        let start = format!(
            r#"{{"ts":"2026-08-02T00:00:00Z","kind":"start","correlation_id":"{child}","parent_conversation":"{face}","goal":"research","model":"m"}}
"#
        );
        std::fs::write(dispatches.join(format!("{child}.jsonl")), start).unwrap();

        let prices = price_table_from_pairs([("m", rates(1.0, 2.0, 0.1))]);
        let report = report_from_data_dir(data, &prices).unwrap();
        let text = format_report(&report);

        assert!(
            text.contains(face),
            "table must show face conversation: {text}"
        );
        assert!(
            !text.contains(&format!("\n{child}")) || report.conversations.len() == 1,
            "child must not be a separate conversation row"
        );
        // The model name itself, not just the word "unpriced" — which also appears as a column
        // header and so matched even when the section was empty.
        assert!(
            report.unpriced.iter().any(|u| u.model == "unpriced"),
            "the model with no rate entry must be listed: {:?}",
            report.unpriced
        );
        assert!(
            text.contains("no usable rate"),
            "the report must print the unpriceable section: {text}"
        );
        assert!(report.conversations[0].cost_usd.is_some());
        assert!(report.roles.iter().any(|r| r.role == "face"));
        assert!(report.roles.iter().any(|r| r.role == "orchestrator"));
        assert!(!report.turn_growth.is_empty());
        assert!(report.cache_hit_rate.is_some());
    }
}
