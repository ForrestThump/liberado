//! # liberado-provider-free-proxy
//!
//! An inference **proxy** over OpenRouter whose single policy is: *only free models, and among
//! them, the one ranking highest on coding benchmarks*. The coding pack already lists OpenRouter
//! as a provider with every model it sells; this crate is the counterpart for unattended runs
//! where the bill is the failure mode — it never forwards a request to a paid model, whatever a
//! config file or a caller asks for.
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
//! api_key_env = "OPENROUTER_API_KEY" # value is ignored by the proxy; set so from_env succeeds
//! ```
//!
//! An in-process `Provider` implementation would have been bypassed by at least one composition
//! root (`acp-bridge` rebuilds an `OpenAiCompatibleProvider` straight from the profile), silently
//! running paid models on that path. A network seam cannot be bypassed by half the system.
//!
//! ## Model selection
//!
//! 1. **Discovery** — `GET /api/v1/models` on OpenRouter is public. A model counts as free only
//!    when both `pricing.prompt` and `pricing.completion` parse to zero.
//! 2. **Ranking** — coding-benchmark scores decide which free model wins.
//!    - Primary: OpenRouter's Benchmarks API (`GET /api/v1/benchmarks?task_type=coding`,
//!      bearer-auth): Artificial Analysis `coding_index`, Design Arena `codecategories` elo, and
//!      τ²-Bench accuracy as tie-breakers, in that order.
//!    - Fallback: when the API cannot answer (no key grant, endpoint gone, rate-limited), scrape
//!      public leaderboard pages through [spider-mcp](https://github.com/ForrestThump/liberado-spider-mcp)
//!      (`POST {SPIDER_MCP_URL}/v1/scrape`) and parse the markdown **deterministically** — table
//!      cells and bounded regexes, no model in the loop anywhere.
//! 3. **Ordering** — free ∩ scored first (composite score descending), then unscored free models
//!    as a safety net: tool-calling capable before not, larger context before smaller.
//!
//! The resolved order refreshes on a TTL (benchmarks are re-scored slowly upstream; the API is
//! rate-limited to 500 requests/day per key) and serves stale data rather than failing when a
//! refresh errors.
//!
//! ## Routing behaviour
//!
//! - Requested model absent / `"auto"` → best-ranked free model.
//! - Requested model names a slug in the current free set → honoured (an explicit choice inside
//!   the mandate).
//! - Requested model names anything else → refused with HTTP 400 naming the nearest ranked free
//!   alternatives. Silently remapping a named model would hide cost intent; refusing says it out
//!   loud.
//! - Upstream refusal for the chosen candidate (429 daily/hourly caps, 402, unknown-model) walks
//!   down the ranked list, up to [`MAX_ATTEMPTS`] candidates per request. Free tiers are capped
//!   per model; failover is how the proxy stays useful at 06:55.
//!
//! ## Environment
//!
//! | Variable | Meaning |
//! |---|---|
//! | `OPENROUTER_API_KEY` | upstream key (free models still require auth); also unlocks the Benchmarks API |
//! | `LIBERADO_FREE_PROXY_BIND` | listen address, default `127.0.0.1:8788` |
//! | `LIBERADO_FREE_PROXY_UPSTREAM_BASE` | default `https://openrouter.ai/api/v1` |
//! | `LIBERADO_FREE_PROXY_TTL_SECS` | ranking cache lifetime, default 21600 (6 h) |
//! | `LIBERADO_FREE_PROXY_MAX_ATTEMPTS` | failover depth, default 3 |
//! | `LIBERADO_FREE_PROXY_SCRAPE_TIMEOUT_SECS` | per-scrape budget, default 90 |
//! | `SPIDER_MCP_URL` | spider-mcp base URL; unset disables the scrape fallback |
//! | `SPIDER_MCP_TOKEN` | bearer for spider-mcp's REST surface, when it runs with one |
//!
//! Bind localhost by default and keep it that way unless you add auth: the proxy trusts its
//! callers and holds an upstream credential.

pub mod bench_api;
pub mod bounded;
pub mod free;
pub mod http;
pub mod match_slug;
pub mod rank;
pub mod resolver;
pub mod scrape_parse;
pub mod service;
pub mod settings;
pub mod spider;

pub use free::FreeModel;
pub use http::router as http_router;
pub use rank::{ModelScores, RankTable};
pub use resolver::{BestFreeModelResolver, DefaultSources, Origin, Resolution};
pub use service::{ProxyConfig, ProxyService};
