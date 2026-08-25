//! Free-model discovery from OpenRouter's public `GET /models` endpoint.
//!
//! Free is a *pricing* fact, not a naming convention. The `:free` slug suffix exists, but the
//! authoritative signal is the model's own pricing table: a model is served by this proxy only
//! when **both** `pricing.prompt` and `pricing.completion` parse to zero. Prices arrive as JSON
//! strings (`"0"`, `"0.0000001"`), so the check parses rather than string-compares — `"0.00"` and
//! `0` must both read as free, and anything unparseable must read as paid (fail closed).

use serde_json::Value;

/// One model OpenRouter currently serves at zero cost.
#[derive(Debug, Clone, PartialEq)]
pub struct FreeModel {
    /// OpenRouter slug, e.g. `z-ai/glm-5.2:free` — what upstream expects in `model`.
    pub id: String,
    /// Advertised context window; `0` when unreported.
    pub context_length: u64,
    /// Whether the model advertises tool calling (`tools` in `supported_parameters`) — the
    /// agentic-coding baseline this proxy exists for.
    pub supports_tools: bool,
}

/// Parse a `/models` response body into its free models.
///
/// Entries missing an id, or with non-zero/unparseable pricing, are skipped rather than failing
/// the whole listing: one malformed row must not take discovery down.
pub fn parse_free_models(body: &Value) -> Vec<FreeModel> {
    let Some(entries) = body["data"].as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries {
        if !is_free_pricing(&e["pricing"]) {
            continue;
        }
        let Some(id) = e["id"].as_str() else { continue };
        out.push(FreeModel {
            id: id.to_string(),
            context_length: e["context_length"].as_u64().unwrap_or(0),
            supports_tools: e["supported_parameters"]
                .as_array()
                .is_some_and(|ps| ps.iter().any(|p| p.as_str() == Some("tools"))),
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// Both per-token rates must exist, parse, and equal zero. Unparseable means paid.
pub fn is_free_pricing(pricing: &Value) -> bool {
    let zero = |field: &str| {
        pricing[field]
            .as_str()
            .and_then(|s| s.trim().parse::<f64>().ok())
            .is_some_and(|v| v == 0.0)
    };
    zero("prompt") && zero("completion")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn zero_priced_models_are_kept_and_sorted() {
        let body = json!({
            "data": [
                { "id": "b/model:free", "context_length": 262144,
                  "pricing": { "prompt": "0", "completion": "0" },
                  "supported_parameters": ["tools", "temperature"] },
                { "id": "a/model:free", "context_length": 65536,
                  "pricing": { "prompt": "0", "completion": "0" },
                  "supported_parameters": [] },
            ]
        });
        let free = parse_free_models(&body);
        assert_eq!(
            free,
            vec![
                FreeModel {
                    id: "a/model:free".into(),
                    context_length: 65536,
                    supports_tools: false
                },
                FreeModel {
                    id: "b/model:free".into(),
                    context_length: 262144,
                    supports_tools: true
                },
            ]
        );
    }

    #[test]
    fn paid_or_unparseable_pricing_is_excluded_fail_closed() {
        let body = json!({
            "data": [
                { "id": "paid", "pricing": { "prompt": "0.0000015", "completion": "0" } },
                { "id": "half-free", "pricing": { "prompt": "0", "completion": "0.000002" } },
                { "id": "garbage", "pricing": { "prompt": "free!", "completion": "0" } },
                { "id": "missing-pricing" },
                { "no-id": true, "pricing": { "prompt": "0", "completion": "0" } },
            ]
        });
        assert!(parse_free_models(&body).is_empty());
    }

    #[test]
    fn zero_spellings_all_read_as_free() {
        for prompt in ["0", "0.0", "0.000000"] {
            assert!(is_free_pricing(
                &json!({ "prompt": prompt, "completion": "0" })
            ));
        }
    }

    #[test]
    fn missing_data_is_empty_not_an_error() {
        assert_eq!(parse_free_models(&json!({})), Vec::new());
        assert_eq!(parse_free_models(&json!({ "data": null })), Vec::new());
    }

    #[test]
    fn duplicate_ids_are_deduped() {
        let body = json!({
            "data": [
                { "id": "x/m", "pricing": { "prompt": "0", "completion": "0" } },
                { "id": "x/m", "pricing": { "prompt": "0", "completion": "0" } },
            ]
        });
        assert_eq!(parse_free_models(&body).len(), 1);
    }

    /// A two-element dedup fixture cannot tell `==` from `!=` in the predicate (both collapse a
    /// pair of identical ids to one); three distinct-plus-one-duplicate entries can.
    #[test]
    fn distinct_models_survive_dedup_in_any_order() {
        let body = json!({
            "data": [
                { "id": "b/m", "pricing": { "prompt": "0", "completion": "0" } },
                { "id": "a/m", "pricing": { "prompt": "0", "completion": "0" } },
                { "id": "c/m", "pricing": { "prompt": "0", "completion": "0" } },
                { "id": "a/m", "pricing": { "prompt": "0", "completion": "0" } },
            ]
        });
        let ids: Vec<String> = parse_free_models(&body).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, vec!["a/m", "b/m", "c/m"]);
    }

    #[test]
    fn context_window_may_be_absent() {
        let body = json!({
            "data": [
                { "id": "x/m", "context_length": null, "pricing": { "prompt": "0", "completion": "0" } },
            ]
        });
        assert_eq!(parse_free_models(&body)[0].context_length, 0);
    }

    /// A parameter list containing *only* `tools` pins the equality itself: any list with extra
    /// entries makes `any(...)` true under both spellings, which is how an inverted comparison
    /// once survived this suite.
    #[test]
    fn tool_support_is_detected_when_tools_is_the_only_parameter() {
        let body = json!({
            "data": [
                { "id": "with/m", "pricing": { "prompt": "0", "completion": "0" },
                  "supported_parameters": ["tools"] },
                { "id": "without/m", "pricing": { "prompt": "0", "completion": "0" },
                  "supported_parameters": ["temperature", "top_p"] },
            ]
        });
        let models = parse_free_models(&body);
        assert_eq!(
            models
                .iter()
                .find(|m| m.id == "with/m")
                .map(|m| m.supports_tools),
            Some(true)
        );
        assert_eq!(
            models
                .iter()
                .find(|m| m.id == "without/m")
                .map(|m| m.supports_tools),
            Some(false)
        );
    }
}
