//! Free-model discovery from OpenAI-compatible `GET /models` endpoints.
//!
//! Free is a *pricing* fact, not a naming convention. The `:free` slug suffix exists, but the
//! authoritative signal is the model's own pricing table when the vendor exposes one:
//!
//! - **OpenRouter / zero-price-required catalogs**: a model is served only when both
//!   `pricing.prompt` and `pricing.completion` parse to zero. Prices arrive as JSON strings
//!   (`"0"`, `"0.0000001"`) or numbers; anything unparseable reads as paid (fail closed).
//! - **Other OpenAI-compat catalogs** that do not expose OpenRouter-style pricing: a listed
//!   model is a free-tier candidate only when there is **no** pricing object, or pricing
//!   parses to zero. Pricing present and non-zero is excluded (do not serve paid leftovers).
//!
//! Public ids are `{provider}/{native}` so Groq `llama-3.1-8b-instant` cannot clobber
//! OpenRouter `meta-llama/llama-3.1-8b-instruct:free`. Chat posts the native id to that
//! vendor's base.

use serde_json::Value;

use crate::providers::{self, CatalogPolicy, ModelAllow, qualify_public_id};

/// One model this proxy currently serves at zero cost.
#[derive(Debug, Clone, PartialEq)]
pub struct FreeModel {
    /// Proxy-facing unique id, e.g. `openrouter/z-ai/glm-5.2:free` or `groq/llama-3.1-8b-instant`.
    pub id: String,
    /// Stable provider slug (`openrouter`, `groq`, …) — selects the upstream base and key.
    pub provider: String,
    /// Native model id that vendor's `/chat/completions` expects.
    pub upstream_id: String,
    /// Advertised context window; `0` when unreported.
    pub context_length: u64,
    /// Whether the model advertises tool calling (`tools` in `supported_parameters`) — the
    /// agentic-coding baseline this proxy exists for.
    pub supports_tools: bool,
}

impl FreeModel {
    /// Fixture constructor used by unit tests: public id equals the native id, provider `test`.
    pub fn fixture(id: impl Into<String>, context_length: u64, supports_tools: bool) -> Self {
        let id = id.into();
        Self {
            provider: "test".into(),
            upstream_id: id.clone(),
            id,
            context_length,
            supports_tools,
        }
    }

    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        let provider = provider.into();
        self.id = qualify_public_id(&provider, &self.upstream_id);
        self.provider = provider;
        self
    }
}

/// Parse an OpenRouter `/models` body: zero-price required, fail closed.
///
/// Entries missing an id, or with non-zero/unparseable pricing, are skipped rather than failing
/// the whole listing: one malformed row must not take discovery down.
pub fn parse_free_models(body: &Value) -> Vec<FreeModel> {
    parse_provider_models(
        body,
        "openrouter",
        CatalogPolicy::ZeroPriceRequired,
        ModelAllow::Chat,
    )
}

/// Parse a `/models` body for one provider under that provider's catalog policy.
pub fn parse_provider_models(
    body: &Value,
    provider: &str,
    policy: CatalogPolicy,
    allow: ModelAllow,
) -> Vec<FreeModel> {
    let Some(entries) = body["data"].as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries {
        if !entry_is_free(e, policy) {
            continue;
        }
        let Some(native) = e["id"].as_str() else {
            continue;
        };
        if !providers::allow_native_id(allow, native) {
            continue;
        }
        out.push(FreeModel {
            id: qualify_public_id(provider, native),
            provider: provider.to_string(),
            upstream_id: native.to_string(),
            context_length: e["context_length"]
                .as_u64()
                .or_else(|| e["context_window"].as_u64())
                .unwrap_or(0),
            supports_tools: detects_tools(e),
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

fn detects_tools(e: &Value) -> bool {
    if e["supported_parameters"]
        .as_array()
        .is_some_and(|ps| ps.iter().any(|p| p.as_str() == Some("tools")))
    {
        return true;
    }
    e["tools"].as_bool() == Some(true)
}

fn entry_is_free(e: &Value, policy: CatalogPolicy) -> bool {
    match e.get("pricing") {
        None | Some(Value::Null) => match policy {
            CatalogPolicy::ZeroPriceRequired => false,
            CatalogPolicy::ZeroPriceOrUnpriced => true,
        },
        Some(pricing) => is_free_pricing(pricing),
    }
}

/// Both per-token rates must exist, parse, and equal zero. Unparseable means paid.
pub fn is_free_pricing(pricing: &Value) -> bool {
    price_is_zero(&pricing["prompt"]) && price_is_zero(&pricing["completion"])
}

fn price_is_zero(v: &Value) -> bool {
    if let Some(s) = v.as_str() {
        return s.trim().parse::<f64>().ok().is_some_and(|n| n == 0.0);
    }
    v.as_f64().is_some_and(|n| n == 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{CatalogPolicy, ModelAllow};
    use crate::quota::BillingKind;
    use serde_json::json;

    fn openrouter(id: &str, ctx: u64, tools: bool) -> FreeModel {
        FreeModel {
            id: format!("openrouter/{id}"),
            provider: "openrouter".into(),
            upstream_id: id.into(),
            context_length: ctx,
            supports_tools: tools,
        }
    }

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
                openrouter("a/model:free", 65536, false),
                openrouter("b/model:free", 262144, true),
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
        assert!(is_free_pricing(&json!({ "prompt": 0, "completion": 0.0 })));
        assert!(!is_free_pricing(
            &json!({ "prompt": 0.000001, "completion": 0 })
        ));
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
        assert_eq!(
            ids,
            vec!["openrouter/a/m", "openrouter/b/m", "openrouter/c/m"]
        );
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
                .find(|m| m.upstream_id == "with/m")
                .map(|m| m.supports_tools),
            Some(true)
        );
        assert_eq!(
            models
                .iter()
                .find(|m| m.upstream_id == "without/m")
                .map(|m| m.supports_tools),
            Some(false)
        );
    }

    #[test]
    fn unpriced_compat_models_are_kept_priced_leftovers_are_not() {
        let body = json!({
            "data": [
                { "id": "llama-3.1-8b-instant", "context_length": 131072 },
                { "id": "paid-sku", "pricing": { "prompt": "0.0001", "completion": "0.0002" } },
                { "id": "whisper-large-v3" },
            ]
        });
        let free = parse_provider_models(
            &body,
            "groq",
            CatalogPolicy::ZeroPriceOrUnpriced,
            ModelAllow::Chat,
        );
        let ids: Vec<&str> = free.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["groq/llama-3.1-8b-instant"]);
        assert_eq!(free[0].upstream_id, "llama-3.1-8b-instant");
        assert_eq!(free[0].provider, "groq");
    }

    #[test]
    fn gemini_pro_never_enters_even_when_unpriced() {
        let body = json!({
            "data": [
                { "id": "gemini-2.5-flash" },
                { "id": "gemini-2.5-pro" },
                { "id": "gemma-4-31b-it" },
            ]
        });
        let free = parse_provider_models(
            &body,
            "gemini",
            CatalogPolicy::ZeroPriceOrUnpriced,
            ModelAllow::GeminiFreeTier,
        );
        let ids: Vec<&str> = free.iter().map(|m| m.upstream_id.as_str()).collect();
        assert_eq!(ids, vec!["gemini-2.5-flash", "gemma-4-31b-it"]);
    }

    #[test]
    fn opencode_paid_catalog_rows_are_dropped() {
        let body = json!({
            "data": [
                { "id": "big-pickle" },
                { "id": "hy3-free" },
                { "id": "glm-5.2", "pricing": { "prompt": "1.40", "completion": "4.40" } },
                { "id": "claude-sonnet-4-5" },
            ]
        });
        let free = parse_provider_models(
            &body,
            "opencode",
            CatalogPolicy::ZeroPriceOrUnpriced,
            ModelAllow::OpenCodeFree,
        );
        let ids: Vec<&str> = free.iter().map(|m| m.upstream_id.as_str()).collect();
        assert_eq!(ids, vec!["big-pickle", "hy3-free"]);
    }

    #[test]
    fn quota_then_pay_billing_kind_is_the_omit_signal() {
        // The adapter layer omits Cloudflare; this pin keeps the kind itself distinct so a
        // future wiring cannot treat it as rate-limited-free by accident.
        assert_ne!(BillingKind::QuotaThenPay, BillingKind::RateLimitedFree);
        assert_ne!(BillingKind::QuotaThenPay, BillingKind::ZeroPricedOnly);
    }
}
