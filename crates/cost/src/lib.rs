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
    ConversationRollup, RoleRollup, TurnGrowth, UnpricedLine, build_report, rollup_conversations,
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
            finish: "stop".into(),
            tool_calls: 0,
            streamed: false,
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
        assert!(text.to_lowercase().contains("unpriced") || text.contains("unpriced"));
        assert!(report.conversations[0].cost_usd.is_some());
        assert!(report.roles.iter().any(|r| r.role == "face"));
        assert!(report.roles.iter().any(|r| r.role == "orchestrator"));
        assert!(!report.turn_growth.is_empty());
        assert!(report.cache_hit_rate.is_some());
    }
}
