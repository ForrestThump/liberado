//! The coding-benchmark rank table and the composite ordering it induces over free models.
//!
//! Scores come from sources with different shapes (the Benchmarks API's per-source items, or
//! scraped leaderboard rows) but one job: say which free model is *best at agentic coding right
//! now*. [`RankTable`] is the merge point — keyed by OpenRouter slug, tolerant of partial data,
//! and ordered by a single documented rule so two runs of the same inputs cannot disagree.

use std::collections::HashMap;

use crate::free::FreeModel;

/// One model's benchmark standing, as far as we could observe it. Every field optional: sources
/// disagree on coverage, and absence must sort as "unranked", never as zero.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelScores {
    /// Artificial Analysis `coding_index` (0–100).
    pub coding_index: Option<f64>,
    /// Design Arena elo for a coding category.
    pub design_arena_elo: Option<f64>,
    /// τ²-Bench accuracy (0–1), multi-turn tool-calling under policy constraints.
    pub tau_accuracy: Option<f64>,
    /// A scraped leaderboard percentage, when the API path was unavailable.
    pub scraped_percent: Option<f64>,
}

/// Slug → observed scores, merged from every source that answered.
#[derive(Debug, Default, Clone)]
pub struct RankTable {
    scores: HashMap<String, ModelScores>,
}

impl RankTable {
    pub fn get(&self, slug: &str) -> Option<&ModelScores> {
        self.scores.get(slug)
    }

    /// Merge `scores` into the table without discarding fields another source already filled.
    pub fn record(&mut self, slug: impl Into<String>, scores: ModelScores) {
        let entry = self.scores.entry(slug.into()).or_default();
        let take = |into: &mut Option<f64>, from: Option<f64>| {
            if from.is_some() {
                *into = from;
            }
        };
        take(&mut entry.coding_index, scores.coding_index);
        take(&mut entry.design_arena_elo, scores.design_arena_elo);
        take(&mut entry.tau_accuracy, scores.tau_accuracy);
        take(&mut entry.scraped_percent, scores.scraped_percent);
    }

    pub fn len(&self) -> usize {
        self.scores.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scores.is_empty()
    }
}

/// Order free models best-coding-first.
///
/// Ranked before unranked; among ranked, `coding_index`, then Design Arena elo, then τ²
/// accuracy, then a scraped leaderboard percent; among unranked, tool-calling capability first,
/// then context window, then slug for total determinism.
pub fn order_free_models(free: &[FreeModel], table: &RankTable) -> Vec<String> {
    let mut ids: Vec<&FreeModel> = free.iter().collect();
    ids.sort_by(|a, b| {
        let sa = table.get(&a.id);
        let sb = table.get(&b.id);
        cmp_ranked(sa.is_some(), sb.is_some())
            .then_with(|| match (sa, sb) {
                (Some(x), Some(y)) => cmp_scores(x, y),
                _ => std::cmp::Ordering::Equal,
            })
            .then_with(|| cmp_ranked(a.supports_tools, b.supports_tools))
            .then_with(|| b.context_length.cmp(&a.context_length))
            .then_with(|| a.id.cmp(&b.id))
    });
    ids.into_iter().map(|m| m.id.clone()).collect()
}

fn cmp_scores(a: &ModelScores, b: &ModelScores) -> std::cmp::Ordering {
    cmp_opt_desc(a.coding_index, b.coding_index)
        .then_with(|| cmp_opt_desc(a.design_arena_elo, b.design_arena_elo))
        .then_with(|| cmp_opt_desc(a.tau_accuracy, b.tau_accuracy))
        .then_with(|| cmp_opt_desc(a.scraped_percent, b.scraped_percent))
}

/// `true` sorts before `false` — ranked beats unranked, tools beat no-tools.
fn cmp_ranked(a: bool, b: bool) -> std::cmp::Ordering {
    b.cmp(&a)
}

fn cmp_opt_desc(a: Option<f64>, b: Option<f64>) -> std::cmp::Ordering {
    // `None` means "no observation", which must lose to any observation — including a low one —
    // so it is pushed past every `Some` regardless of value.
    match (a, b) {
        (Some(x), Some(y)) => y.partial_cmp(&x).unwrap_or(std::cmp::Ordering::Equal),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, ctx: u64, tools: bool) -> FreeModel {
        FreeModel::fixture(id, ctx, tools)
    }

    fn scored(coding: Option<f64>) -> ModelScores {
        ModelScores {
            coding_index: coding,
            ..Default::default()
        }
    }

    #[test]
    fn coding_index_decides_among_scored_models() {
        let free = vec![
            model("weak/m", 100_000, true),
            model("strong/m", 10_000, true),
        ];
        let mut table = RankTable::default();
        table.record("weak/m", scored(Some(51.0)));
        table.record("strong/m", scored(Some(78.4)));
        assert_eq!(order_free_models(&free, &table), vec!["strong/m", "weak/m"]);
    }

    #[test]
    fn any_score_beats_an_unranked_model_even_a_bigger_one() {
        let free = vec![
            model("big/unranked", 1_000_000, true),
            model("small/scored", 32_000, true),
        ];
        let mut table = RankTable::default();
        table.record("small/scored", scored(Some(9.0)));
        assert_eq!(
            order_free_models(&free, &table),
            vec!["small/scored", "big/unranked"]
        );
    }

    #[test]
    fn unscored_models_fall_back_to_tools_then_context_then_slug() {
        let free = vec![
            model("z/plain", 1_000_000, false),
            model("y/tools", 128_000, true),
            model("x/tools-bigger", 256_000, true),
            model("w/tools-biggest", 256_000, true),
        ];
        assert_eq!(
            order_free_models(&free, &RankTable::default()),
            vec!["w/tools-biggest", "x/tools-bigger", "y/tools", "z/plain"]
        );
    }

    #[test]
    fn design_arena_and_tau_break_ties_when_coding_index_is_absent() {
        let free = vec![model("a/m", 0, true), model("b/m", 0, true)];
        let mut table = RankTable::default();
        table.record(
            "a/m",
            ModelScores {
                design_arena_elo: Some(1100.0),
                tau_accuracy: Some(0.2),
                ..Default::default()
            },
        );
        table.record(
            "b/m",
            ModelScores {
                design_arena_elo: Some(1050.0),
                tau_accuracy: Some(0.9),
                ..Default::default()
            },
        );
        assert_eq!(order_free_models(&free, &table), vec!["a/m", "b/m"]);
    }

    #[test]
    fn a_scraped_percent_ranks_when_the_api_answered_nothing() {
        let free = vec![
            model("api-less/m", 0, true),
            model("other/m", 999_999, true),
        ];
        let mut table = RankTable::default();
        table.record(
            "api-less/m",
            ModelScores {
                scraped_percent: Some(64.2),
                ..Default::default()
            },
        );
        assert_eq!(
            order_free_models(&free, &table),
            vec!["api-less/m", "other/m"]
        );
    }

    #[test]
    fn record_merges_without_discarding_other_sources_fields() {
        let mut table = RankTable::default();
        table.record("m", scored(Some(40.0)));
        table.record(
            "m",
            ModelScores {
                scraped_percent: Some(70.0),
                ..Default::default()
            },
        );
        let s = table.get("m").unwrap();
        assert_eq!(s.coding_index, Some(40.0));
        assert_eq!(s.scraped_percent, Some(70.0));
    }

    #[test]
    fn empty_inputs_order_to_empty() {
        assert!(order_free_models(&[], &RankTable::default()).is_empty());
        let free = vec![model("only/m", 8_000, false)];
        assert_eq!(
            order_free_models(&free, &RankTable::default()),
            vec!["only/m"]
        );
    }

    #[test]
    fn len_and_is_empty_report_table_population() {
        let mut table = RankTable::default();
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);
        table.record("a/m", scored(Some(1.0)));
        assert!(!table.is_empty());
        assert_eq!(table.len(), 1);
    }
}
