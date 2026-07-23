# Diagnosis: why turbomcp can't share spider-mcp's binary

> ## ⚠️ CORRECTION (2026-07-17, later same day): this conclusion was WRONG.
> The scrape hang is **NOT caused by turbomcp.** When I later built a **fresh hand-rolled** image
> (no turbomcp at all) from the proven homelab source, it hung **identically** — same "Raw
> single-page scrape timed out after 29999 ms," same tcpdump signature (TCP connects, data flows,
> spider hangs *processing*). The real variable is a **fresh build**: the 2-week-old *deployed* image
> (`compose-spider-mcp`, built 2026-06-27) scrapes fine, but **every image built today** — turbomcp
> or hand-rolled — hangs. **Every turbomcp experiment below was also a fresh build, so turbomcp was
> confounded with "rebuilt-today."** I never ran the control (a fresh hand-rolled build) until after
> writing this doc — a textbook missing-control error (failure-modes doctrine). The likely true cause
> is a transitive dependency bump or Chrome 149→150 that landed after 2026-06-27; it needs a
> dependency/Chrome bisect. See `goal.md` → S1 for the live status. The turbomcp-specific analysis
> below is **retained only as a record of what was tested**, not as a valid conclusion.

**Status:** post-mortem, 2026-07-17 (**superseded — see correction above**). Decision at the time was
"spider-mcp stays hand-rolled." That's still where it runs (on the proven deployed image), but the
*reason* was misattributed to turbomcp.

## TL;DR

Porting `spider-mcp` to turbomcp (to standardize its MCP surface) **breaks all scraping**: every
fetch hangs for the full 30s budget and times out. It is **not a version conflict** — spider and
turbomcp use the *same* versions of every shared crate (`reqwest 0.13.4`, `hyper`, `tokio`, `rustls`,
…) and they agree. It is an **interaction in the single unified binary**: adding turbomcp changes the
combined build of the shared HTTP/async stack (Cargo feature unification is global + additive, and the
port also bumped axum 0.7→0.8 and tower-http 0.5→0.6), and something in that combined build stalls
spider's **response processing**. The obvious one-binary levers (toggle a reqwest feature, swap the
TLS backend, change thread count) provably do **not** fix it. A real fix would require either an
instrumented deep-dive to find the exact stall, or **process isolation** (two binaries). For a
one-binary/one-process goal, that means: don't force turbomcp onto spider.

## What it is NOT (ruled out by experiment)

Every row was a real test on the homelab, not a guess.

| Hypothesis | How tested | Result |
|---|---|---|
| reqwest `http2` feature | turbomcp branch dropping it; rebuild + scrape | ❌ still hangs |
| rustls vs native-tls backend | swap workspace reqwest `rustls→native-tls` + flip the one `.use_rustls_tls()` | ❌ still hangs |
| any reqwest feature turbomcp adds | strip reqwest to `json,stream,native-tls`; `cargo tree` confirms spider already pulls the rest | ❌ still hangs |
| missing rustls `CryptoProvider` | install `aws_lc_rs` default provider at `main()` start | ❌ still hangs |
| runtime thread starvation | A/B: cgroup `cpus:0.5`→1 tokio worker vs no-limit→4 workers | ❌ **both** hang identically |
| TLS handshake / connectivity / IPv6 | **tcpdump inside the container during a hung scrape** | ❌ ruled out — network is healthy (see below) |
| response decompression (zstd/brotli/gzip) | lockfile diff old-vs-new | ❌ compression crates **identical** in both |

## The decisive evidence: the network works

A packet capture (alpine + tcpdump sharing the container's netns) during a hung scrape of
`https://example.com` shows the connection is **completely healthy**:

- TCP handshake completes (SYN → SYN-ACK → ACK),
- spider's TLS ClientHello goes out (a 1759-byte record — the spoofed Chrome fingerprint is sent),
- the server returns ServerHello + certificate,
- application data flows **both directions**,
- zero IPv6 connection attempts (example.com resolves to IPv6 in-container, but reqwest correctly
  uses IPv4).

**So the bytes arrive. spider hangs *after* receiving the response — in its own body-assembly /
post-scrape processing — not in connecting or TLS.** This overturned the initial "TLS handshake hang"
reading; the stall is application-level. The `io_uring unavailable → tokio::fs fallback` log lines
fire immediately before the freeze, so spider's post-response file writes (session store, content
cache, diagnostics) under the unified async runtime are the most likely place it wedges — but this was
not isolated to a single line.

## Why one binary can't paper over it

Cargo compiles **one** copy of each dependency for a binary, with the **union** of all requested
features; features are additive and cannot be subtracted. So within `spider-mcp + turbomcp` you cannot
give spider's `reqwest`/`hyper`/`tokio` a different build than turbomcp's — they share one. That's why
no feature toggle on turbomcp's side changed spider's resolved build (spider already pulls the
superset). The only way to give the two components genuinely different builds is to put them in
**separate compilation units that don't link together** — i.e. separate processes.

## The unrelated bug this surfaced (now fixed)

The long-standing "spider 401" that made Liberado unable to reach spider was **not** the turbomcp
issue and **not** an auth-design flaw. Compose set `SPIDER_MCP_TOKEN=${FIRECRAWL_API_KEY}`, but
`FIRECRAWL_API_KEY` is unset in `.env`, so the container received `SPIDER_MCP_TOKEN=""` — *present but
empty*. The hand-rolled server treats an **unset** token as "auth disabled (allow all)" but an
**empty-string** token as "auth enabled, expected token is ''", so every Liberado request (no bearer)
got 401. Fix: remove the env line so the token is truly unset. `/mcp` now returns 200, and Liberado
connects to the working hand-rolled scraper. (A code-level hardening —
`std::env::var("SPIDER_MCP_TOKEN").ok().filter(|t| !t.is_empty())` — would make this impossible to
reintroduce.)

## Recommendation / if revisited

- **Keep spider-mcp hand-rolled.** It is deployed, connected, and scraping.
- If turbomcp-for-spider is ever wanted anyway, do **not** retry feature toggles — go straight to
  either (a) a `tokio-console`/tracing-instrumented build to find which task stops being polled after
  the response arrives, or (b) **process isolation**: a thin turbomcp MCP front that proxies to the
  spider scraper over localhost, so the two never share a compiled binary.
- Repo hygiene follow-up: `liberado-spider-mcp` `master` currently holds the broken turbomcp port;
  the deployment uses the local hand-rolled image. To restore build-from-GitHub for spider, revert
  `master` to a **branded hand-rolled** version (keep the rename + Dockerfile, drop turbomcp) and add
  the empty-token guard above. Experiment branches (`exp/*` on both repos) are left intact.
