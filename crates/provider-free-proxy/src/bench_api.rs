//! OpenRouter's Benchmarks API — the primary ranking source.
//!
//! `GET {base}/benchmarks?task_type=coding` (bearer-auth, 30 req/min and 500/day per key) returns
//! a unified item list whose shape differs per `source`:
//!
//! - `artificial-analysis` → `coding_index` (0–100), the strongest signal we have;
//! - `design-arena` → `elo`, filtered here to coding categories (`codecategories`);
//! - `openrouter` → τ²-Bench / GPQA items; only τ² accuracy is kept, as a tie-breaker.
//!
//! This module parses that list into [`ModelScores`] rows. It deliberately ignores every other
//! field: pricing arrives from `/models` (the discovery path's authority), and anything not
//! coding-shaped must not influence the ordering.

use serde_json::Value;

use crate::rank::ModelScores;

/// Parse a unified Benchmarks API response into slug → scores.
///
/// Rows without a usable `model_permaslug` are skipped; unknown sources are ignored rather than
/// rejected so a new upstream source cannot take ranking down.
pub fn parse_benchmarks(body: &Value) -> Vec<(String, ModelScores)> {
    let Some(items) = body["data"].as_array() else {
        return Vec::new();
    };
    let mut out: Vec<(String, ModelScores)> = Vec::new();
    for item in items {
        let Some(slug) = item["model_permaslug"].as_str() else {
            continue;
        };
        let source = item["source"].as_str().unwrap_or_default();
        let mut scores = ModelScores::default();
        match source {
            "artificial-analysis" => {
                scores.coding_index = finite(item["coding_index"].as_f64());
            }
            "design-arena" => {
                // `task_type=coding` maps Design Arena to its code categories upstream; the check
                // is repeated here so an unfiltered response still yields only coding rows.
                let coding = item["category"]
                    .as_str()
                    .is_some_and(|c| c.to_ascii_lowercase().contains("code"));
                if coding {
                    scores.design_arena_elo = finite(item["elo"].as_f64());
                }
            }
            "openrouter" => {
                // τ²-Bench is the agentic tool-use benchmark; GPQA and search items carry other
                // meanings under the same source tag, so gate on the benchmark type.
                if item["benchmark_type"].as_str() == Some("tau_bench_verified_airline") {
                    scores.tau_accuracy = finite(item["accuracy"].as_f64());
                }
            }
            _ => {}
        }
        if has_any_score(&scores) {
            merge_into(&mut out, slug, scores);
        }
    }
    out
}

fn has_any_score(s: &ModelScores) -> bool {
    s.coding_index.is_some()
        || s.design_arena_elo.is_some()
        || s.tau_accuracy.is_some()
        || s.scraped_percent.is_some()
}

fn merge_into(out: &mut Vec<(String, ModelScores)>, slug: &str, scores: ModelScores) {
    if let Some((_, existing)) = out.iter_mut().find(|(s, _)| s == slug) {
        if scores.coding_index.is_some() {
            existing.coding_index = scores.coding_index;
        }
        if scores.design_arena_elo.is_some() {
            existing.design_arena_elo = scores.design_arena_elo;
        }
        if scores.tau_accuracy.is_some() {
            existing.tau_accuracy = scores.tau_accuracy;
        }
        if scores.scraped_percent.is_some() {
            existing.scraped_percent = scores.scraped_percent;
        }
    } else {
        out.push((slug.to_string(), scores));
    }
}

/// A score must be a real number to count; NaN/∞ would poison every later comparison.
fn finite(v: Option<f64>) -> Option<f64> {
    v.filter(|f| f.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn artificial_analysis_coding_index_is_read() {
        let body = json!({
            "meta": { "as_of": "2026-08-23T00:00:00Z", "version": "v1" },
            "data": [
                { "source": "artificial-analysis", "model_permaslug": "z-ai/glm-5.2",
                  "display_name": "GLM", "intelligence_index": 60.0,
                  "coding_index": 71.4, "agentic_index": null },
                { "source": "artificial-analysis", "model_permaslug": "x/m",
                  "coding_index": null },
            ]
        });
        let rows = parse_benchmarks(&body);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "z-ai/glm-5.2");
        assert_eq!(rows[0].1.coding_index, Some(71.4));
    }

    #[test]
    fn design_arena_counts_only_coding_categories() {
        let body = json!({
            "data": [
                { "source": "design-arena", "model_permaslug": "a/m",
                  "category": "codecategories", "elo": 1180.0 },
                { "source": "design-arena", "model_permaslug": "b/m",
                  "category": "gamedev", "elo": 1300.0 },
            ]
        });
        let rows = parse_benchmarks(&body);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "a/m");
    }

    #[test]
    fn tau_accuracy_is_kept_and_other_openrouter_benchmarks_ignored() {
        let body = json!({
            "data": [
                { "source": "openrouter", "model_permaslug": "t/m",
                  "benchmark_type": "tau_bench_verified_airline", "accuracy": 0.42 },
                { "source": "openrouter", "model_permaslug": "g/m",
                  "benchmark_type": "gpqa_diamond", "accuracy": 0.9 },
            ]
        });
        let rows = parse_benchmarks(&body);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "t/m");
        assert_eq!(rows[0].1.tau_accuracy, Some(0.42));
    }

    #[test]
    fn same_slug_across_sources_merges_into_one_row() {
        let body = json!({
            "data": [
                { "source": "artificial-analysis", "model_permaslug": "m/m", "coding_index": 50.0 },
                { "source": "design-arena", "model_permaslug": "m/m",
                  "category": "codecategories", "elo": 1200.0 },
            ]
        });
        let rows = parse_benchmarks(&body);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1.coding_index, Some(50.0));
        assert_eq!(rows[0].1.design_arena_elo, Some(1200.0));
    }

    #[test]
    fn malformed_rows_are_skipped_not_fatal() {
        let body = json!({
            "data": [
                { "source": "artificial-analysis", "coding_index": 99.0 }, // no slug
                { "model_permaslug": "u/m", "coding_index": 10.0 },        // no source → ignored
                { "source": "brand-new-source", "model_permaslug": "v/m", "score": 7.0 },
            ]
        });
        assert!(parse_benchmarks(&body).is_empty());
    }

    #[test]
    fn missing_data_is_empty() {
        assert!(parse_benchmarks(&json!({})).is_empty());
    }
}
