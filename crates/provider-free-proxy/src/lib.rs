//! # liberado-provider-free-proxy
//!
//! An inference **proxy** over genuinely-free OpenAI-compatible endpoints whose single policy
//! is: *only free models, and among them, the one ranking highest on coding benchmarks*. The
//! coding pack already lists OpenRouter as a provider with every model it sells; this crate is
//! the counterpart for unattended runs where the bill is the failure mode — it never forwards
//! a request to a paid model, a paid-after-quota SKU, or an endpoint that starts billing when
//! a token cap is exceeded.
//!
//! ## Why a proxy, not another `Provider` impl
//!
//! Every consumer of inference in Liberado (the daemon's role providers, the coding pack's
//! `CoderRoleProviderFactory`, `coder-runner`'s direct mode, the ACP bridge) already knows how to
//! talk to an OpenAI-compatible `[[providers]]` profile. A proxy that *speaks that wire format*
//! therefore becomes available everywhere by adding one TOML entry — no changes to bootstrap,
//! config-loader, coder-runner or acp-bridge:
//!
//! ```toml
//! [[providers]]
//! name = "free-coding"
//! base_url = "http://127.0.0.1:8788/v1"
//! default_model = "auto"             # the proxy resolves "auto" to the best-ranked free model
//! api_key_env = "LIBERADO_FREE_PROXY_PLACEHOLDER"
//! ```
//!
//! The proxy ignores the caller's key. It boots when **at least one** provider env key below
//! is set, and it never requires OpenRouter specifically.
//!
//! An in-process `Provider` implementation would have been bypassed by at least one composition
//! root (`acp-bridge` rebuilds an `OpenAiCompatibleProvider` straight from the profile), silently
//! running paid models on that path. A network seam cannot be bypassed by half the system.
//!
//! ## Model selection
//!
//! 1. **Discovery** — every configured adapter is asked for `GET {base}/models`.
//!    - **OpenRouter** (and any zero-price-required catalog): a model counts as free only when
//!      both `pricing.prompt` and `pricing.completion` parse to zero. Unparseable = paid.
//!    - **Other OpenAI-compat catalogs** that do not expose that pricing object: listed models
//!      are free-tier candidates only when there is no pricing object, *or* pricing parses to
//!      zero. Pricing present and non-zero is excluded.
//!    - **Documented free-tier allowlists**: Gemini keeps Flash / Flash-Lite / Gemma and skips
//!      Pro (quota-then-pay once billing is enabled). OpenCode Zen keeps models the public
//!      pricing table lists as Free (`-free` suffix, `big-pickle`). AnyAPI keeps OpenRouter-style
//!      `:free` suffixes and still requires zero `pricing.prompt` + `pricing.completion`.
//!    - A provider that 401/403/timeouts during discovery is skipped with a warning, not a hard
//!      error, as long as **some** provider produced a non-empty free set.
//!    - Cloudflare Workers AI is recognized by env name but **omitted**: neuron billing is
//!      quota-then-pay and remaining quota cannot be bounded without inventing a billing
//!      framework. Prefer omitting over guessing.
//!
//!    Public ids are `{provider}/{native}` so two vendors cannot clobber each other.
//! 2. **Ranking** — coding-benchmark scores decide which free model is tried first. This is
//!    first-class, not a fallback: the walk is always best-remaining-first.
//!    - Primary: OpenRouter's Benchmarks API (`GET /api/v1/benchmarks?task_type=coding`,
//!      bearer-auth, when OpenRouter is configured): Artificial Analysis `coding_index`,
//!      Design Arena `codecategories` elo, and τ²-Bench accuracy as tie-breakers, in that
//!      order. Scores map onto proxy-facing ids / OpenRouter slugs the way [`match_slug`]
//!      already does.
//!    - Fallback: when the API cannot answer (no key grant, endpoint gone, rate-limited), scrape
//!      public leaderboard pages through [spider-mcp](https://github.com/ForrestThump/liberado-spider-mcp)
//!      (`POST {SPIDER_MCP_URL}/v1/scrape`) and parse the markdown **deterministically** — table
//!      cells and bounded regexes, no model in the loop anywhere.
//! 3. **Ordering** — free ∩ scored first (composite score descending), then unscored free models
//!    as a safety net: tool-calling capable before not, larger context before smaller. Paid
//!    leftovers never enter the ranked list. An empty merged catalog is a hard error.
//!
//! The resolved order refreshes on a TTL (benchmarks are re-scored slowly upstream; the API is
//! rate-limited to 500 requests/day per key) and serves stale data rather than failing when a
//! refresh errors on transport. An empty free catalog is a hard error: yesterday's ranking is
//! not reused, because those slugs may now be paid.
//!
//! ## Routing behaviour
//!
//! - Requested model absent / `"auto"` → best-ranked free model, then fall through the ranking.
//! - Requested model names a slug in the current free set → honoured alone (an explicit choice
//!   inside the mandate; no walk to the next-best model).
//! - Requested model names anything else → refused with HTTP 400 naming the nearest ranked free
//!   alternatives. Silently remapping a named model would hide cost intent; refusing says it out
//!   loud.
//! - Upstream refusal for the chosen candidate (429 / rate-limit, 5xx, timeout, candidate-scoped
//!   transport error, 402, 403, unknown-model) walks down the ranked list to the next-best remaining
//!   free model, up to [`DEFAULT_MAX_ATTEMPTS`](crate::settings::DEFAULT_MAX_ATTEMPTS)
//!   candidates per request. Each attempt uses **that model's** provider base and key. A 400
//!   payload error does not spend another candidate. Quota-then-pay candidates are skipped
//!   *before* a request when remaining quota is unknown or would not cover it.
//!
//! ## Environment
//!
//! Provider keys (values are never logged). Skip a provider when its env is empty/whitespace.
//! Boot requires at least one of these **names** to be set; none set is a fail-fast listing
//! the names, not the values.
//!
//! | Variable | Known OpenAI-compat base | Catalog rule |
//! |---|---|---|
//! | `OPENROUTER_API_KEY` | `https://openrouter.ai/api/v1` | both `pricing.prompt` and `pricing.completion` parse to 0; also unlocks the Benchmarks API |
//! | `GROQ_API_KEY` | `https://api.groq.com/openai/v1` | rate-limited free; unpriced `/models` rows are candidates, priced non-zero excluded |
//! | `GEMINI_API_KEY` | `https://generativelanguage.googleapis.com/v1beta/openai` | Flash / Flash-Lite / Gemma only; Pro skipped |
//! | `MISTRAL_API_KEY` | `https://api.mistral.ai/v1` | zero-price required (pay-per-token catalog otherwise empty) |
//! | `NVIDIA_AI_API_KEY` | `https://integrate.api.nvidia.com/v1` | rate-limited playground; unpriced chat rows are candidates |
//! | `CEREBRAS_API_KEY` | `https://api.cerebras.ai/v1` | zero-price required (list prices are pay-per-token; 402 is not a free SKU) |
//! | `OPENCODE_ZEN_API_KEY` | `https://opencode.ai/zen/v1` | documented Free SKUs only (`-free`, `big-pickle`) |
//! | `CLOUDFLARE_WORKERS_API_KEY` | `https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1` | **omitted** (neuron quota-then-pay). Also needs `CLOUDFLARE_ACCOUNT_ID`; missing account id skips the adapter |
//! | `KILOCODE_API_KEY` | `https://api.kilo.ai/api/gateway` | zero-price required |
//! | `ANYAPI_API_KEY` | `https://api.anyapi.ai/v1` | zero-price required **and** native id ends in `:free` |
//!
//! | Variable | Meaning |
//! |---|---|
//! | `LIBERADO_FREE_PROXY_BIND` | listen address, default `127.0.0.1:8788` |
//! | `LIBERADO_FREE_PROXY_UPSTREAM_BASE` | OpenRouter base override, default `https://openrouter.ai/api/v1` |
//! | `LIBERADO_FREE_PROXY_{PROVIDER}_BASE` | optional base override per adapter (`GROQ`, `GEMINI`, `NVIDIA`, …) |
//! | `LIBERADO_FREE_PROXY_TTL_SECS` | ranking cache lifetime, default 21600 (6 h) |
//! | `LIBERADO_FREE_PROXY_MAX_ATTEMPTS` | failover depth, default 6 |
//! | `LIBERADO_FREE_PROXY_SCRAPE_TIMEOUT_SECS` | per-scrape budget, default 90 |
//! | `SPIDER_MCP_URL` | spider-mcp base URL; unset disables the scrape fallback |
//! | `SPIDER_MCP_TOKEN` | bearer for spider-mcp's REST surface, when it runs with one |
//!
//! Bind localhost by default and keep it that way unless you add auth: the proxy trusts its
//! callers and holds upstream credentials.

pub mod bench_api;
pub mod bounded;
pub mod free;
pub mod http;
pub mod match_slug;
pub mod providers;
pub mod quota;
pub mod rank;
pub mod resolver;
pub mod scrape_parse;
pub mod service;
pub mod settings;
pub mod spider;

pub use free::FreeModel;
pub use http::router as http_router;
pub use providers::{
    BillingKind, Upstream, UpstreamRegistry, any_listed_key_set, configured_upstreams,
    listed_key_env_names,
};
pub use rank::{ModelScores, RankTable};
pub use resolver::{BestFreeModelResolver, DefaultSources, Origin, Resolution};
pub use service::{ProxyConfig, ProxyService, RouteCandidate};
