# liberado-provider-free-proxy

An OpenAI-compatible inference proxy (`liberado-free-proxy`) that serves **only
genuinely free** models and tries the **best coding model first**.

It never forwards a request to a paid model, a paid-after-quota SKU, or an
endpoint that starts billing when a daily/monthly token cap is exceeded.

## Ranking (best-coding-first)

The walk is always best-remaining-first. It is not round-robin and not an LLM
judge.

1. **Scored free models first**, using OpenRouter's Benchmarks API when that
   provider is configured: Artificial Analysis `coding_index`, then Design Arena
   coding elo, then τ²-Bench accuracy, then a scraped leaderboard percent.
2. Scores map onto proxy-facing ids / OpenRouter slugs the way `match_slug`
   already does. Scrapes never re-introduce paid OpenRouter slugs.
3. **Unscored free models** sit on the heuristic floor: tools-capable first,
   then larger context, then slug.
4. An empty free catalog is a hard error. Yesterday's ranking is not reused
   when the catalog is empty, because those slugs may now be paid.

Send `"model": "auto"` (or omit `model`) for that ranked fall-through. An
explicit slug is honoured only if it is currently free, and it does **not**
walk to the next-best model.

On 403 / 429 / rate-limit / 5xx / timeout / candidate-scoped transport error /
quota exhaustion, the proxy falls through to the next-best remaining free
model, up to `LIBERADO_FREE_PROXY_MAX_ATTEMPTS` (default 6). Each attempt uses
**that model's** provider base and key. A 400 payload error does not spend
another candidate.

## Providers

Boot if **at least one** of the env names below is set. None set is a fail-fast
that lists the **names**, never values. Keys are resolved only in memory and
never logged.

| Env | Base | Catalog |
|---|---|---|
| `OPENROUTER_API_KEY` | `https://openrouter.ai/api/v1` | `pricing.prompt` and `pricing.completion` both parse to 0 |
| `GROQ_API_KEY` | `https://api.groq.com/openai/v1` | rate-limited free |
| `GEMINI_API_KEY` | `https://generativelanguage.googleapis.com/v1beta/openai` | Flash / Flash-Lite / Gemma; Pro skipped |
| `MISTRAL_API_KEY` | `https://api.mistral.ai/v1` | zero-price required |
| `NVIDIA_AI_API_KEY` | `https://integrate.api.nvidia.com/v1` | rate-limited playground |
| `CEREBRAS_API_KEY` | `https://api.cerebras.ai/v1` | zero-price required |
| `OPENCODE_ZEN_API_KEY` | `https://opencode.ai/zen/v1` | documented Free SKUs only |
| `CLOUDFLARE_WORKERS_API_KEY` | `https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1` | **omitted** (neuron quota-then-pay). Needs `CLOUDFLARE_ACCOUNT_ID` |
| `KILOCODE_API_KEY` | `https://api.kilo.ai/api/gateway` | zero-price required |
| `ANYAPI_API_KEY` | `https://api.anyapi.ai/v1` | zero-price required and `:free` suffix |

Public catalog ids are `{provider}/{native}` so two vendors cannot clobber each
other. `/v1/models` lists those proxy-facing ids; chat rewrites `model` to the
native id for the chosen vendor.

Other knobs: `LIBERADO_FREE_PROXY_BIND`, `LIBERADO_FREE_PROXY_UPSTREAM_BASE`
(OpenRouter override), `LIBERADO_FREE_PROXY_TTL_SECS`,
`LIBERADO_FREE_PROXY_MAX_ATTEMPTS`, `LIBERADO_FREE_PROXY_SCRAPE_TIMEOUT_SECS`,
`SPIDER_MCP_URL`, `SPIDER_MCP_TOKEN`. See the crate rustdoc table.
