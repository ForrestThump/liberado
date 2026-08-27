//! Upstream adapters: env **names**, documented OpenAI-compat bases, and billing policy.
//!
//! Keys are resolved only in memory and never logged. An unset, empty, or whitespace env
//! skips that adapter. A vendor that is pay-per-token with no documented free SKU, or that
//! starts billing after a quota, is omitted from the catalog rather than guessed at.

use crate::quota::QuotaBudget;

pub use crate::quota::BillingKind;

/// How `/models` rows become free-tier candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogPolicy {
    /// Both `pricing.prompt` and `pricing.completion` must parse to 0. Fail closed.
    ZeroPriceRequired,
    /// No pricing object → treat as a free-tier candidate. Pricing present → same zero-price
    /// filter (non-zero / unparseable excluded).
    ZeroPriceOrUnpriced,
}

/// Optional extra filter on native model ids (documented free-tier SKUs only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelAllow {
    /// Any chat-shaped id that passes the pricing policy.
    Chat,
    /// Google AI Studio free-tier chat SKUs: Flash / Flash-Lite / Gemma. Pro is skipped
    /// (quota-then-pay once billing is enabled).
    GeminiFreeTier,
    /// OpenCode Zen models documented as Free (suffix `-free`, or `big-pickle`).
    OpenCodeFree,
    /// AnyAPI mixed catalog: OpenRouter-style ids whose native id ends in `:free`.
    AnyApiFree,
}

/// One configured upstream. The resolved key lives only here.
#[derive(Clone)]
pub struct Upstream {
    /// Stable slug used as the public-id prefix (`groq`, `openrouter`, …).
    pub id: String,
    pub base_url: String,
    /// Env **name** the key came from — for diagnostics, never a value.
    pub api_key_env: String,
    api_key: String,
    pub catalog: CatalogPolicy,
    pub billing: BillingKind,
    pub allow: ModelAllow,
    pub quota: QuotaBudget,
}

impl std::fmt::Debug for Upstream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upstream")
            .field("id", &self.id)
            .field("base_url", &self.base_url)
            .field("api_key_env", &self.api_key_env)
            .field("api_key", &"<redacted>")
            .field("catalog", &self.catalog)
            .field("billing", &self.billing)
            .field("allow", &self.allow)
            .field("quota", &self.quota)
            .finish()
    }
}

impl Upstream {
    /// Bearer token. Callers must not log this.
    pub fn bearer(&self) -> &str {
        &self.api_key
    }

    /// Whether sending one more request is known not to bill.
    pub fn may_send(&self) -> bool {
        self.quota.allows_request(self.billing)
    }

    /// Build an adapter. Production wiring goes through [`configured_upstreams`].
    pub fn from_parts(
        id: impl Into<String>,
        base_url: impl Into<String>,
        api_key_env: impl Into<String>,
        api_key: impl Into<String>,
        catalog: CatalogPolicy,
        billing: BillingKind,
        allow: ModelAllow,
    ) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into(),
            api_key_env: api_key_env.into(),
            api_key: api_key.into(),
            catalog,
            billing,
            allow,
            quota: QuotaBudget::unknown(),
        }
    }

    /// Test/fixture constructor.
    pub fn new_for_tests(
        id: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        billing: BillingKind,
    ) -> Self {
        let catalog = match billing {
            BillingKind::ZeroPricedOnly => CatalogPolicy::ZeroPriceRequired,
            _ => CatalogPolicy::ZeroPriceOrUnpriced,
        };
        Self::from_parts(
            id,
            base_url,
            "TEST_API_KEY",
            api_key,
            catalog,
            billing,
            ModelAllow::Chat,
        )
    }
}

/// Lookup table keyed by provider slug. Debug redacts keys.
#[derive(Clone, Default)]
pub struct UpstreamRegistry {
    by_id: std::collections::HashMap<String, Upstream>,
}

impl std::fmt::Debug for UpstreamRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.by_id.iter()).finish()
    }
}

impl UpstreamRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_upstreams(upstreams: impl IntoIterator<Item = Upstream>) -> Self {
        let mut by_id = std::collections::HashMap::new();
        for up in upstreams {
            by_id.insert(up.id.clone(), up);
        }
        Self { by_id }
    }

    pub fn insert(&mut self, upstream: Upstream) {
        self.by_id.insert(upstream.id.clone(), upstream);
    }

    pub fn get(&self, id: &str) -> Option<&Upstream> {
        self.by_id.get(id)
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Upstream> {
        self.by_id.values()
    }
}

struct Spec {
    id: &'static str,
    api_key_env: &'static str,
    account_id_env: Option<&'static str>,
    default_base: &'static str,
    catalog: CatalogPolicy,
    billing: BillingKind,
    allow: ModelAllow,
}

/// Documented first-wave adapters. Cloudflare is listed so its env names appear in the
/// fail-fast message, but [`BillingKind::QuotaThenPay`] omits it from the catalog.
const SPECS: &[Spec] = &[
    Spec {
        id: "openrouter",
        api_key_env: "OPENROUTER_API_KEY",
        account_id_env: None,
        default_base: "https://openrouter.ai/api/v1",
        catalog: CatalogPolicy::ZeroPriceRequired,
        billing: BillingKind::ZeroPricedOnly,
        allow: ModelAllow::Chat,
    },
    Spec {
        id: "groq",
        api_key_env: "GROQ_API_KEY",
        account_id_env: None,
        default_base: "https://api.groq.com/openai/v1",
        catalog: CatalogPolicy::ZeroPriceOrUnpriced,
        billing: BillingKind::RateLimitedFree,
        allow: ModelAllow::Chat,
    },
    Spec {
        id: "gemini",
        api_key_env: "GEMINI_API_KEY",
        account_id_env: None,
        default_base: "https://generativelanguage.googleapis.com/v1beta/openai",
        catalog: CatalogPolicy::ZeroPriceOrUnpriced,
        billing: BillingKind::RateLimitedFree,
        allow: ModelAllow::GeminiFreeTier,
    },
    Spec {
        id: "mistral",
        api_key_env: "MISTRAL_API_KEY",
        account_id_env: None,
        default_base: "https://api.mistral.ai/v1",
        catalog: CatalogPolicy::ZeroPriceRequired,
        billing: BillingKind::ZeroPricedOnly,
        allow: ModelAllow::Chat,
    },
    Spec {
        id: "nvidia",
        api_key_env: "NVIDIA_AI_API_KEY",
        account_id_env: None,
        default_base: "https://integrate.api.nvidia.com/v1",
        catalog: CatalogPolicy::ZeroPriceOrUnpriced,
        billing: BillingKind::RateLimitedFree,
        allow: ModelAllow::Chat,
    },
    Spec {
        id: "cerebras",
        api_key_env: "CEREBRAS_API_KEY",
        account_id_env: None,
        default_base: "https://api.cerebras.ai/v1",
        // Authenticated `/v1/models` lists SKUs with no pricing object; the public catalog
        // prices them pay-per-token and chat returns 402 payment_required. Do not treat
        // unpriced leftovers as free.
        catalog: CatalogPolicy::ZeroPriceRequired,
        billing: BillingKind::ZeroPricedOnly,
        allow: ModelAllow::Chat,
    },
    Spec {
        id: "opencode",
        api_key_env: "OPENCODE_ZEN_API_KEY",
        account_id_env: None,
        default_base: "https://opencode.ai/zen/v1",
        catalog: CatalogPolicy::ZeroPriceOrUnpriced,
        billing: BillingKind::RateLimitedFree,
        allow: ModelAllow::OpenCodeFree,
    },
    Spec {
        id: "cloudflare",
        api_key_env: "CLOUDFLARE_WORKERS_API_KEY",
        account_id_env: Some("CLOUDFLARE_ACCOUNT_ID"),
        default_base: "https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1",
        catalog: CatalogPolicy::ZeroPriceOrUnpriced,
        billing: BillingKind::QuotaThenPay,
        allow: ModelAllow::Chat,
    },
    Spec {
        id: "kilocode",
        api_key_env: "KILOCODE_API_KEY",
        account_id_env: None,
        default_base: "https://api.kilo.ai/api/gateway",
        catalog: CatalogPolicy::ZeroPriceRequired,
        billing: BillingKind::ZeroPricedOnly,
        allow: ModelAllow::Chat,
    },
    Spec {
        id: "anyapi",
        api_key_env: "ANYAPI_API_KEY",
        account_id_env: None,
        default_base: "https://api.anyapi.ai/v1",
        // Mixed paid + free. Keep only `$0` rows whose native id ends in `:free`.
        catalog: CatalogPolicy::ZeroPriceRequired,
        billing: BillingKind::ZeroPricedOnly,
        allow: ModelAllow::AnyApiFree,
    },
];

/// Env **names** (never values) the binary lists when none are set.
pub fn listed_key_env_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = SPECS.iter().map(|s| s.api_key_env).collect();
    names.push("CLOUDFLARE_ACCOUNT_ID");
    names
}

/// Whether any listed provider key has non-whitespace content.
pub fn any_listed_key_set(lookup: impl Fn(&str) -> Option<String>) -> bool {
    SPECS
        .iter()
        .any(|s| nonempty(lookup(s.api_key_env).as_deref()))
}

/// Build adapters for every provider whose key is set and that is safe to call at $0.
///
/// Cloudflare is recognized (and skipped when the account id is missing) but omitted from
/// the returned set: neuron billing is quota-then-pay and we will not guess remaining quota.
pub fn configured_upstreams(lookup: impl Fn(&str) -> Option<String>) -> Vec<Upstream> {
    let mut out = Vec::new();
    for spec in SPECS {
        let Some(api_key) = lookup(spec.api_key_env).filter(|s| nonempty(Some(s))) else {
            continue;
        };
        if let Some(extra) = spec.account_id_env
            && lookup(extra).filter(|s| nonempty(Some(s))).is_none()
        {
            tracing::warn!(
                provider = spec.id,
                missing = extra,
                "skipping adapter: extra env unset"
            );
            continue;
        }
        if spec.billing == BillingKind::QuotaThenPay {
            tracing::warn!(
                provider = spec.id,
                env = spec.api_key_env,
                "omitted from the free catalog: quota-then-pay cannot be billed-safely"
            );
            continue;
        }

        let base_url = base_for(spec, &lookup);

        out.push(Upstream {
            id: spec.id.to_string(),
            base_url,
            api_key_env: spec.api_key_env.to_string(),
            api_key,
            catalog: spec.catalog,
            billing: spec.billing,
            allow: spec.allow,
            quota: QuotaBudget::unknown(),
        });
    }
    out
}

fn nonempty(raw: Option<&str>) -> bool {
    raw.is_some_and(|s| !s.trim().is_empty())
}

fn base_for(spec: &Spec, lookup: &impl Fn(&str) -> Option<String>) -> String {
    // OpenRouter keeps the historical override name.
    if spec.id == "openrouter"
        && let Some(over) =
            lookup("LIBERADO_FREE_PROXY_UPSTREAM_BASE").filter(|s| nonempty(Some(s)))
    {
        return over;
    }
    let specific = format!("LIBERADO_FREE_PROXY_{}_BASE", spec.id.to_ascii_uppercase());
    lookup(&specific)
        .filter(|s| nonempty(Some(s)))
        .unwrap_or_else(|| spec.default_base.to_string())
}

/// Whether `native_id` is a chat-shaped free-tier candidate under `allow`.
pub fn allow_native_id(allow: ModelAllow, native_id: &str) -> bool {
    if looks_like_non_chat(native_id) {
        return false;
    }
    match allow {
        ModelAllow::Chat => true,
        ModelAllow::GeminiFreeTier => gemini_is_documented_free_tier(native_id),
        ModelAllow::OpenCodeFree => opencode_is_documented_free(native_id),
        ModelAllow::AnyApiFree => anyapi_is_documented_free(native_id),
    }
}

/// Embeddings, ASR, TTS, rerank, and guard models are not chat completions.
pub fn looks_like_non_chat(id: &str) -> bool {
    let s = id.to_ascii_lowercase();
    [
        "embed",
        "rerank",
        "whisper",
        "tts",
        "guard",
        "moderation",
        "imagen",
        "veo",
        "aqa",
    ]
    .iter()
    .any(|needle| s.contains(needle))
}

/// Google AI Studio free-tier chat SKUs. Pro is skipped: with billing enabled it is
/// quota-then-pay.
pub fn gemini_is_documented_free_tier(id: &str) -> bool {
    let id = id.trim_start_matches("models/").to_ascii_lowercase();
    if looks_like_non_chat(&id) {
        return false;
    }
    if id.starts_with("gemma") {
        return true;
    }
    if id.contains("pro") {
        return false;
    }
    id.contains("flash")
}

/// OpenCode Zen models the public pricing table lists as Free.
pub fn opencode_is_documented_free(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    id.ends_with("-free") || id == "big-pickle"
}

/// AnyAPI free SKUs use OpenRouter-style ids with a `:free` suffix.
pub fn anyapi_is_documented_free(id: &str) -> bool {
    id.to_ascii_lowercase().ends_with(":free")
}

/// Public catalog id: `{provider}/{native}` unless `native` is already prefixed.
pub fn qualify_public_id(provider: &str, native_id: &str) -> String {
    let prefix = format!("{provider}/");
    if native_id.starts_with(&prefix) {
        native_id.to_string()
    } else {
        format!("{provider}/{native_id}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup_of<'a>(map: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            map.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn empty_whitespace_and_unset_keys_are_skipped() {
        let ups = configured_upstreams(lookup_of(&[
            ("OPENROUTER_API_KEY", ""),
            ("GROQ_API_KEY", "   "),
        ]));
        assert!(ups.is_empty());
        assert!(!any_listed_key_set(lookup_of(&[("GROQ_API_KEY", "  ")])));
    }

    #[test]
    fn a_single_non_openrouter_key_is_enough() {
        let ups = configured_upstreams(lookup_of(&[("GROQ_API_KEY", "gsk-test")]));
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0].id, "groq");
        assert_eq!(ups[0].base_url, "https://api.groq.com/openai/v1");
        assert_eq!(ups[0].bearer(), "gsk-test");
        assert!(any_listed_key_set(lookup_of(&[(
            "GROQ_API_KEY",
            "gsk-test"
        )])));
    }

    #[test]
    fn a_provider_specific_base_override_is_honoured() {
        let ups = configured_upstreams(lookup_of(&[
            ("GROQ_API_KEY", "gsk-test"),
            (
                "LIBERADO_FREE_PROXY_GROQ_BASE",
                "http://127.0.0.1:9/openai/v1",
            ),
        ]));
        assert_eq!(ups[0].base_url, "http://127.0.0.1:9/openai/v1");
    }

    #[test]
    fn openrouter_base_override_is_honoured() {
        let ups = configured_upstreams(lookup_of(&[
            ("OPENROUTER_API_KEY", "sk-or"),
            (
                "LIBERADO_FREE_PROXY_UPSTREAM_BASE",
                "http://127.0.0.1:9/api/v1",
            ),
        ]));
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0].base_url, "http://127.0.0.1:9/api/v1");
    }

    #[test]
    fn cloudflare_without_account_id_is_skipped() {
        let ups = configured_upstreams(lookup_of(&[("CLOUDFLARE_WORKERS_API_KEY", "cf-token")]));
        assert!(ups.is_empty());
    }

    #[test]
    fn cloudflare_with_account_id_is_still_omitted_as_quota_then_pay() {
        let ups = configured_upstreams(lookup_of(&[
            ("CLOUDFLARE_WORKERS_API_KEY", "cf-token"),
            ("CLOUDFLARE_ACCOUNT_ID", "acct-1"),
        ]));
        assert!(
            ups.is_empty(),
            "neuron billing must not enter the catalog: {ups:?}"
        );
        assert!(any_listed_key_set(lookup_of(&[(
            "CLOUDFLARE_WORKERS_API_KEY",
            "cf-token"
        )])));
    }

    #[test]
    fn mistral_and_kilo_are_wired_but_zero_price_required() {
        let ups = configured_upstreams(lookup_of(&[
            ("MISTRAL_API_KEY", "ms-test"),
            ("KILOCODE_API_KEY", "kilo-test"),
        ]));
        assert_eq!(ups.len(), 2);
        assert!(
            ups.iter()
                .all(|u| u.catalog == CatalogPolicy::ZeroPriceRequired)
        );
    }

    #[test]
    fn cerebras_is_zero_price_required() {
        let ups = configured_upstreams(lookup_of(&[("CEREBRAS_API_KEY", "csk-test")]));
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0].id, "cerebras");
        assert_eq!(ups[0].catalog, CatalogPolicy::ZeroPriceRequired);
        assert_eq!(ups[0].billing, BillingKind::ZeroPricedOnly);
    }

    #[test]
    fn debug_does_not_print_the_key() {
        let up = Upstream::new_for_tests(
            "groq",
            "http://x",
            "super-secret-key",
            BillingKind::RateLimitedFree,
        );
        let rendered = format!("{up:?}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(
            !rendered.contains("super-secret-key"),
            "key leaked in Debug: {rendered}"
        );
    }

    #[test]
    fn gemini_allowlist_keeps_flash_and_gemma_skips_pro() {
        assert!(gemini_is_documented_free_tier("gemini-2.5-flash"));
        assert!(gemini_is_documented_free_tier(
            "models/gemini-2.5-flash-lite"
        ));
        assert!(gemini_is_documented_free_tier("gemma-4-31b-it"));
        assert!(!gemini_is_documented_free_tier("gemini-2.5-pro"));
        assert!(!gemini_is_documented_free_tier("gemini-3.1-pro"));
        assert!(!gemini_is_documented_free_tier("imagen-3.0"));
    }

    #[test]
    fn opencode_allowlist_is_documented_free_only() {
        assert!(opencode_is_documented_free("big-pickle"));
        assert!(opencode_is_documented_free("hy3-free"));
        assert!(opencode_is_documented_free("nemotron-3-ultra-free"));
        assert!(!opencode_is_documented_free("glm-5.2"));
        assert!(!opencode_is_documented_free("claude-sonnet-4-5"));
    }

    #[test]
    fn anyapi_empty_key_is_skipped() {
        assert!(configured_upstreams(lookup_of(&[("ANYAPI_API_KEY", "")])).is_empty());
        assert!(configured_upstreams(lookup_of(&[("ANYAPI_API_KEY", "   ")])).is_empty());
        assert!(!any_listed_key_set(lookup_of(&[("ANYAPI_API_KEY", "")])));
    }

    #[test]
    fn anyapi_is_zero_price_required_with_free_suffix() {
        let ups = configured_upstreams(lookup_of(&[("ANYAPI_API_KEY", "sk-anyapi")]));
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0].id, "anyapi");
        assert_eq!(ups[0].base_url, "https://api.anyapi.ai/v1");
        assert_eq!(ups[0].catalog, CatalogPolicy::ZeroPriceRequired);
        assert_eq!(ups[0].billing, BillingKind::ZeroPricedOnly);
        assert_eq!(ups[0].allow, ModelAllow::AnyApiFree);
        assert_eq!(ups[0].api_key_env, "ANYAPI_API_KEY");
    }

    #[test]
    fn anyapi_allowlist_requires_colon_free_suffix() {
        assert!(anyapi_is_documented_free(
            "meta-llama/llama-3.3-70b-instruct:free"
        ));
        assert!(anyapi_is_documented_free("Vendor/Model:FREE"));
        assert!(!anyapi_is_documented_free("openai/gpt-4o"));
        assert!(!anyapi_is_documented_free("hy3-free"));
    }

    #[test]
    fn listed_names_cover_every_adapter_env() {
        let names = listed_key_env_names();
        for expected in [
            "OPENROUTER_API_KEY",
            "GROQ_API_KEY",
            "GEMINI_API_KEY",
            "MISTRAL_API_KEY",
            "NVIDIA_AI_API_KEY",
            "CEREBRAS_API_KEY",
            "OPENCODE_ZEN_API_KEY",
            "CLOUDFLARE_WORKERS_API_KEY",
            "CLOUDFLARE_ACCOUNT_ID",
            "KILOCODE_API_KEY",
            "ANYAPI_API_KEY",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
    }

    #[test]
    fn qualify_does_not_double_prefix() {
        assert_eq!(
            qualify_public_id("groq", "llama-3.1-8b-instant"),
            "groq/llama-3.1-8b-instant"
        );
        assert_eq!(
            qualify_public_id("groq", "groq/llama-3.1-8b-instant"),
            "groq/llama-3.1-8b-instant"
        );
    }
}
