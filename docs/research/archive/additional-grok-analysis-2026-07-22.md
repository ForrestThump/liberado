
Architectural hardening review

I re-read the 2026-07-22 analysis against the live tree (crates/, architecture docs, roadmap), then measured with line counts, llvm-cov, and cargo mutants --list. Scope is Liberado only — nested MCP repos ignored as requested.

Short version: the analysis is still directionally right. Seams and layer rules are the load-bearing good news. Complexity risk is concentrated in a few composition-root / lifecycle blobs, MCP lifecycle, and tests that prove the wrong thing or never touch production paths. Highest ROI is not a rewrite — it is T1 live conformance + MCP pooling + targeted module splits + mutation/coverage gates on the kernel.

───

1. Verdict on the existing analysis

┌─────────────────────────────────┬────────────────────────────────┬─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ Finding                         │ Status now                     │ Notes                                                                                                           │
├─────────────────────────────────┼────────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ MCP fresh connect per execution │ Still true                     │ factory.rs + ARCHITECTURE.md: no pool                                                                           │
├─────────────────────────────────┼────────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ Telegram in composition root    │ Partially improved             │ ChatSurface exists; TelegramChatBridge implements it — but sticky, cron delivery, and glue still live in server │
├─────────────────────────────────┼────────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ Chat tests on non-prod store    │ Mostly fixed                   │ JsonlStore deleted; chat lens tests hit SessionStore. Residual dual is GoalSessionStore (see below)             │
├─────────────────────────────────┼────────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ config-loader god model         │ Still true                     │ model.rs ~2,038 lines                                                                                           │
├─────────────────────────────────┼────────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ common holds live catalog       │ Still true                     │ catalog.rs ~464 lines; docs still claim purity                                                                  │
├─────────────────────────────────┼────────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ Fleet pin file                  │ Still open                     │ ops debt, not code shape                                                                                        │
├─────────────────────────────────┼────────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ Hot-path god files              │ Still true                     │ measured below                                                                                                  │
├─────────────────────────────────┼────────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ Latency not closed-loop         │ Still open                     │ journal exists; policy from p95 does not                                                                        │
├─────────────────────────────────┼────────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ Security holes                  │ Structural / defer consciously │ correct framing                                                                                                 │
├─────────────────────────────────┼────────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ Docs >> scoreboard              │ Still true                     │ ~88 docs markdown files; T1 suite still “planned”                                                               │
└─────────────────────────────────┴────────────────────────────────┴─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘

Correction to §3.3: the JsonlStore false-confidence class was fixed (documented in conversation-store and session-store/tests/conversation_lens.rs). Do not re-spend budget there. The remaining dual is different: kernel hub tests still use the intentional in-memory GoalSessionStore (~35 call sites) while production boots SessionStore.

That dual is acknowledged in session/src/store.rs (“obviously a double”), and record_lens.rs dual-exercises both. Risk is lower than JsonlStore — but hub/cancel/list/resume behavior can still be “green” only on the double. That is still failure-mode #1 wearing a thinner coat.

───

2. Quantitative picture (what the tools said)

Largest production files

┌────────────────────────────┬──────┬───────────────────────────────────────────────────┐
│ File                       │ ~LOC │ Role                                              │
├────────────────────────────┼──────┼───────────────────────────────────────────────────┤
│ daemon/src/lib.rs          │ 2507 │ react + proposals + pools + cron delivery helpers │
├────────────────────────────┼──────┼───────────────────────────────────────────────────┤
│ executor/src/lib.rs        │ 2400 │ agent loop + budgets                              │
├────────────────────────────┼──────┼───────────────────────────────────────────────────┤
│ config-loader/src/model.rs │ 2038 │ entire TOML surface                               │
├────────────────────────────┼──────┼───────────────────────────────────────────────────┤
│ server/src/api.rs          │ 1512 │ full HTTP/SSE surface                             │
├────────────────────────────┼──────┼───────────────────────────────────────────────────┤
│ dispatcher/src/lib.rs      │ 1288 │ classify + guidance                               │
├────────────────────────────┼──────┼───────────────────────────────────────────────────┤
│ orchestrator/src/lib.rs    │ 1191 │ execute decisions                                 │
└────────────────────────────┴──────┴───────────────────────────────────────────────────┘

llvm-cov (line cover, selected packages)

Strong (do not gold-plate):

┌─────────────────────────┬─────────┐
│ Area                    │   Cover │
├─────────────────────────┼─────────┤
│ dispatcher guards + lib │ ~96–99% │
├─────────────────────────┼─────────┤
│ common (most modules)   │ ~90–98% │
├─────────────────────────┼─────────┤
│ executor                │    ~96% │
├─────────────────────────┼─────────┤
│ session-store JSONL     │    ~90% │
├─────────────────────────┼─────────┤
│ mcp multi/scoped        │    100% │
├─────────────────────────┼─────────┤
│ daemon overall          │    ~90% │
└─────────────────────────┴─────────┘

Thin (where complexity will collapse first):

┌─────────────────────────────┬───────┬────────────────────────────────┐
│ Area                        │ Cover │ Why it matters                 │
├─────────────────────────────┼───────┼────────────────────────────────┤
│ server/src/api.rs           │  ~56% │ every surface talks here       │
├─────────────────────────────┼───────┼────────────────────────────────┤
│ server/src/telegram.rs      │    0% │ primary phone surface          │
├─────────────────────────────┼───────┼────────────────────────────────┤
│ server/src/lib.rs           │    0% │ composition root / wiring      │
├─────────────────────────────┼───────┼────────────────────────────────┤
│ server/src/latency.rs       │    0% │ journal path untested          │
├─────────────────────────────┼───────┼────────────────────────────────┤
│ server/src/cron_delivery.rs │  ~48% │ daily-driver delivery path     │
├─────────────────────────────┼───────┼────────────────────────────────┤
│ mcp/src/connector.rs        │    0% │ real stdio/HTTP transports     │
├─────────────────────────────┼───────┼────────────────────────────────┤
│ session/src/hub.rs          │  ~83% │ cancel/list/history of mutants │
├─────────────────────────────┼───────┼────────────────────────────────┤
│ session/src/runner.rs       │  ~79% │ park/resume                    │
└─────────────────────────────┴───────┴────────────────────────────────┘

Critical-path aggregate for common/mcp/session-store/dispatcher: ~94%.
Hot daemon/session/executor/server aggregate: ~78% — almost entirely dragged down by server surface glue.

cargo mutants inventory (viable mutants, list-only)

┌───────────────────┬────────────────┐
│ Package           │ Mutants listed │
├───────────────────┼────────────────┤
│ liberado-server   │           ~204 │
├───────────────────┼────────────────┤
│ liberado-executor │           ~175 │
├───────────────────┼────────────────┤
│ liberado-session  │           ~151 │
├───────────────────┼────────────────┤
│ liberado-daemon   │            ~75 │
├───────────────────┼────────────────┤
│ liberado-mcp      │            ~49 │
└───────────────────┴────────────────┘

You already learned on session (2026-07-14): ~37% miss rate, including no-op cancel and empty list. That campaign should become a periodic gate, not a one-off war story.

Dependency / layer health

• layer_rules.rs is doing real work (pack containment, surface thinness, foundation purity, ≤8 internal deps for non-roots).
• server is a root with 21 internal deps — expected for a composition root, but it is also becoming a product surface (Telegram + sticky + cron fold + API). That is the maintainability smell: roots may be wide, but they should not own multi-channel product logic.
• Surfaces (TUI) stay clean on client crates. Keep that.

───

3. What is architecturally solid (do not “improve” away)

1. Narrow waists are real — Provider / ToolRuntime / EventSource / DomainPackRunner / dual session lenses / wire contract / CapabilitySet.
2. Mechanical layer rules — better than prose audits; extend them carefully rather than inventing new abstraction crates.
3. Daemon-first star topology — pools without peer mesh is still correct.
4. Failure-modes doctrine — still the best review checklist you have.
5. Conversation-store as trait-only — correct post-D7 shape; implementation lives in SessionStore.
6. Opaque pack config sections — the config-loader ↔ coder-core inversion was the right class of fix.

These are the skeleton. Hardening should protect them, not replace them.

───

4. Hardening plan (ordered by leverage)

This is deliberately pre-feature work that makes the next features cheaper.

Wave A — stop the next silent production break (1–2 weeks of focus)

A1. Build T1 live conformance (highest architectural insurance)

docs/roadmap/live-conformance-suite.md is still planned, not built. That is the single best maintainability investment: every expensive defect you found was “green unit suite, dead on real daemon.”

• Real liberado-server on temp port + temp data dir
• MockProvider only (CI-able)
• Assert ground truth (backend saw answer, write refused, cancel terminal) not narration
• Start with L6, L8, L2/L3, L1 from that doc — they map directly to failure-modes #1/#2/#3

This is modularity insurance: it freezes behavior of the composition root without freezing internal file layout.

A2. MCP connection pool + degraded catalog (M1)

Still the P1 reliability tax. Design that stays modular:

┌─────────────────────────────────────┬────────────────────────────────────────────────────────┐
│ Piece                               │ Placement                                              │
├─────────────────────────────────────┼────────────────────────────────────────────────────────┤
│ Pool keyed by (mcp_name, transport) │ liberado-mcp (McpRegistry / connectors)                │
├─────────────────────────────────────┼────────────────────────────────────────────────────────┤
│ Idle TTL + reconnect-on-error       │ same                                                   │
├─────────────────────────────────────┼────────────────────────────────────────────────────────┤
│ Critical warm-connect at boot       │ bootstrap/topology flag                                │
├─────────────────────────────────────┼────────────────────────────────────────────────────────┤
│ Degraded peer state                 │ catalog (CapabilityCatalog or thin status channel)     │
├─────────────────────────────────────┼────────────────────────────────────────────────────────┤
│ Dispatcher avoids dead peers        │ consume catalog health, don’t invent a second registry │
└─────────────────────────────────────┴────────────────────────────────────────────────────────┘

Do not put pool policy in server. Keep it under the MCP/RuntimeFactory waist so orchestrator and dispatch packs benefit automatically.

Also: connector.rs is at 0% cover — pooling work is a natural place to add connector-level tests (mock transports already exist for multi/scoped).

A3. Mutation-testing campaign on the kernel (not the whole monorepo)

Prioritize catch-rate, not coverage vanity:

liberado-session  (hub cancel/list/park/resume)
liberado-executor (budget / loop guards / doom)
liberado-dispatcher (downgrade-only guards)
liberado-mcp (scoped invoke, factory allow-list)
liberado-session-store (append/fork/rehydrate)

Practical rules you already know and should codify:

• Commit first; avoid reckless --in-place without a clean tree
• Always --test-workspace=true so cross-crate tests count
• Target: catch ≥ 85% of viable mutants on session hub + dispatcher guards before declaring “hardened”
• Optional CI: weekly mutants job, or gate only packages under a mutant-miss budget

A4. Kill dual-store false confidence for hub behavior

Not “delete GoalSessionStore” (layering needs a kernel-side double). Instead:

1. For every load-bearing hub behavior (cancel, list, park→answer→resume, fork, rehydrate), add or move a test that runs the hub against SessionStore (as record_lens already duals for the store trait).
2. Keep the in-memory double for pure unit tests of pack logic.
3. Optional mechanical check: a small test or script that fails if new hub tests only construct GoalSessionStore::new() without a sibling SessionStore case for cancel/list/resume.

That is the modern form of “point tests at the production object.”

───

Wave B — modularity without a rewrite (structural weight reduction)

B1. Split god files by lifecycle modules first, crates later

Follow modularity.md: extract crates only when a second consumer forces it.

daemon/src/lib.rs (~2.5k) → modules like:

• watch / debounce (exists)
• react (process_change → session)
• proposals (sign / archive / permission)
• pools (authority segregation)
• cron_delivery helpers (or leave delivery policy at server adapter)

executor/src/lib.rs (~2.4k) →:

• loop / budget / doom / report / tool_run
• keep risk_gated.rs as the decorator seam it already is

server/src/api.rs (~1.5k) → route groups matching docs/reference/api.md:

• chat, sessions/goals, catalog/models, hooks (already separate), search, status

This alone reduces merge conflict surface and makes T1 tests easier to aim.

B2. Finish messaging extraction (thin server)

You already have the right traits (MessagingChannel, ChatSurface). Incomplete migration is the risk.

┌───────────────────────────────────────┬────────────────────────────────────────────────────────────────────────────────────────┐
│ Move out of server                    │ Into                                                                                   │
├───────────────────────────────────────┼────────────────────────────────────────────────────────────────────────────────────────┤
│ TelegramChatBridge + sticky id policy │ telegram-surface crate or expand telegram-approvals / thin server module behind traits │
├───────────────────────────────────────┼────────────────────────────────────────────────────────────────────────────────────────┤
│ Cron fold-in adapter                  │ messaging/notifier adapter (already half there via ChatDeliveringNotifier)             │
├───────────────────────────────────────┼────────────────────────────────────────────────────────────────────────────────────────┤
│ Keep in server                        │ AppState, router, hub assembly, bootstrap                                              │
└───────────────────────────────────────┴────────────────────────────────────────────────────────────────────────────────────────┘

Success criterion: a second channel (Matrix) is a new adapter crate, not a second telegram.rs.

B3. Split config-loader model by section

Not a redesign of ChainLoader — file/module split:

• topology.rs (MCPs, pools, schedules, hooks, vault)
• policy.rs (zones, grants)
• tuning.rs (dispatch/context/concurrency/telegram/cron)
• keep pack sections opaque (toml::Value)

Velocity win: every new feature stops editing a 2k-line file and fighting unrelated merge noise.

B4. Catalog purity (incremental, not big-bang)

When you touch catalog for MCP degraded state (A2), that is the friction trigger to lift live registry to liberado-catalog (or service role crate). Leave pure vocabulary in common. Update the ARCHITECTURE claim when you do — narration outrunning code is failure-mode #3.

───

Wave C — quality gates so complexity can’t silently return

┌───────────────────────────┬────────────────────────────────────────────┬─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ Gate                      │ Tool                                       │ Suggestion                                                                                                      │
├───────────────────────────┼────────────────────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ Layer rules               │ already in cargo test                      │ keep; maybe add “roots may not depend on pack X unless…” only if needed                                         │
├───────────────────────────┼────────────────────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ Line coverage floor       │ cargo llvm-cov                             │ start with packages: common, dispatcher, session-store, executor, mcp — floor 90% lines                         │
├───────────────────────────┼────────────────────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ Composition-root coverage │ llvm-cov                                   │ server won’t hit 90% soon; instead require T1 suite green + cover critical handlers (goals message/cancel/fork) │
├───────────────────────────┼────────────────────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ Mutation budget           │ cargo mutants                              │ session + dispatcher monthly; fail on miss of named critical mutants (cancel, list, scope check)                │
├───────────────────────────┼────────────────────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ Unused deps               │ cargo machete / cargo deny (you have deny) │ periodic hygiene, not every PR                                                                                  │
├───────────────────────────┼────────────────────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ God-file budget           │ simple script                              │ warn if any non-surface src/**/*.rs exceeds e.g. 1200 lines without an open split PR                            │
└───────────────────────────┴────────────────────────────────────────────┴─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘

Avoid a monorepo-wide 100% coverage cult. Kernel correctness + composition-root conformance is the Liberado-shaped bar.

───

Wave D — ops / security (don’t pretend these are code refactors)

• Fleet pin file for TurboVault + peer MCP git revs (deploy + diagnosis truth)
• One LAN policy: deliberate unauth + network boundary or optional HTTP bearer on MCP client
• OS sandbox for MCP children: track as constraint, not near-term fake TODO
• One-line ops scoreboard in handoff.md (already almost there): morning brief status, MCP set, sticky restart — archive plans that don’t drive the next commit

───

5. Explicit “do not do” list (hardening edition)

1. No peer agent mesh / A2A — still rejected for good reasons.
2. No absorbing nested MCPs into the Cargo workspace — pin revs; path-deps for co-dev.
3. No big-bang rewrite of common or “micro-crate everything” — module splits first.
4. No new traits without a second consumer — ChatClient is the cautionary tale.
5. No deleting GoalSessionStore just to force SessionStore into the kernel layer (that would invert layering).
6. No gold-plating TUI / coder-agent / heuristics-tuner during this hardening window — large, but not what collapses unattended life-ops.
7. Don’t expand docs volume to “document the hardening” — one short plan + scoreboard is enough.

───

6. Suggested sequencing (concrete)

Week 1
  ├── A1 scaffold: boot real server + MockProvider in tests/ (L8 cancel, L6 write grant)
  ├── A3 mutants on liberado-session + liberado-dispatcher (baseline miss list)
  └── B1 start: split server/api.rs into route modules (no behavior change)

Week 2
  ├── A2 MCP pool + health in catalog (feature-flag or topology flag)
  ├── A4 dual hub tests on SessionStore for cancel/list/resume
  └── B2 move TelegramChatBridge behind messaging crate boundary (even if still in-tree module)

Week 3
  ├── A1 expand T1 (park/resume, fork, cron→dispatch session)
  ├── B3 split config-loader model modules
  └── C llvm-cov floor on kernel packages in CI

Then resume P1 features (C1 AskHuman crons, registry UX) on a stiffer substrate.

───

7. Open suggestions beyond the original analysis

1. Composition-root test strategy — unit tests of pure functions in daemon/executor; integration tests only for server wiring. Don’t try to unit-test server/src/lib.rs into 90% cover.
2. webui/target and nested build artifacts under crates — hygiene tax; ensure gitignore/clean so architecture tools and rust-analyzer stay sane.
3. Role of conversation-store — keep as pure contract; consider a layer_rules note that store implementations of chat lens must live in session-store (already true in practice).
4. Budget “battery” from latency journal — after pool lands, close the loop so flapping tools don’t burn unattended runs into PartiallySucceeded forever.
5. Mechanical “narration audit” — grepping for present-tense claims in Progress strings / doc comments is cheap insurance against failure-mode #3; could be a tiny CI script later.

───

8. Bottom line

Your architecture won’t collapse from missing abstractions — it will collapse from:

1. Unattended edges (MCP cold-connect / flaky peers) without pool + degraded state
2. Composition-root bulk (server as second product) without messaging finish + API module split
3. Tests that miss production objects (hub double vs SessionStore; 0% Telegram/connector)
4. No live conformance gate while unit coverage looks fine

The 2026-07-22 analysis was right on priority: M1 pooling, finish messaging, kill dual confidence. After measuring, I’d promote T1 live conformance and a mutants campaign on session/dispatcher to the same “Now” tier as pooling — they are the hardening equivalent of layer_rules for behavior.

If you want a next step in-repo, I can either:

• turn this into a short docs/roadmap/architecture-hardening.md with a checkable scoreboard, or
• start implementing Wave A (T1 scaffold + MCP pool design, or module splits on api.rs / daemon) — your call which PR you want first.