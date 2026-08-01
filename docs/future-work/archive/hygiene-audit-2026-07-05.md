# Hygiene audit — dedup, coupling, decomposition, anti-patterns (2026-07-05)

Three parallel subagent passes across the whole `crates/` workspace, split by layer: the core
reactive/safety pipeline (`common`, `daemon`, `dispatcher`, `orchestrator`, `executor`, `vault`), the
config/integration/infrastructure layer (`config`, `config-loader`, `bootstrap`, `mcp`, `mcp-forge`,
`provider*`, `cron`, `notify`, `telegram-approvals`, `heuristics-tuner`, `eval`), and the client-facing
layer (`server`, `main-agent`, `conversation-store`, `webui`, `tui`, `cli`, `chat-client-contract`,
`liberado-commands`, `chat-search`, `chat-search-mcp`). Priorities reflect actual risk/payoff, not raw
finding count. **Status: fixes are being applied in the order presented below — each item gets a
"Resolution" paragraph once addressed, so this doc doubles as a progress tracker, not just a snapshot.**

## Priority 1 — worth fixing soon

### `static mut` in the webui's SSE handling is genuinely unsound, not just ugly

`crates/webui/src/components/chat.rs:424` (plus five more `unsafe` blocks at 464/560/580/644/657):
a `static mut CURRENT_SOURCE: Option<Rc<web_sys::EventSource>>` is read and written across six sites.
The comments justify it as "browser-only, single-threaded WASM," but a shared mutable static is UB
under Rust's aliasing rules regardless of thread count, and the raw-pointer dereference at line 560
(mutating a `ThinkingStep` through a pointer while `messages` is also borrowed mutably) is fragile if
the message `Vec` ever reallocates between the `find_map` and the dereference. Fix: `thread_local! {
static CURRENT_SOURCE: RefCell<Option<Rc<EventSource>>> = ... }` — the conventional WASM pattern,
removes every `unsafe` block.

**Resolution (2026-07-05):** Replaced the `static mut` with `thread_local! { static CURRENT_SOURCE:
RefCell<Option<Rc<EventSource>>> = const { RefCell::new(None) }; }`; all six call sites now go
through `.with(|cell| ...)` instead of `unsafe`. Separately fixed the raw-pointer dereference in the
`tool_result` handler: it used to `find_map` a `*mut ThinkingStep` and dereference it later via
`unsafe { &mut *ptr }`; rewritten as a single `.filter(...).find_map(...)` chain that returns a plain
`&mut ThinkingStep` borrowed directly from `messages`, no pointer or `unsafe` needed at all — NLL
handles using it immediately after just fine. Zero `unsafe` blocks remain in `chat.rs`. Verified with
`dx build --package liberado-webui --web` (the actual wasm32 build path this crate uses in
production — `cargo check --target wasm32-unknown-unknown` hit an unrelated local toolchain/target
resolution issue, so `dx build` was used as the authoritative check instead).

### The safety-critical guard pipeline is implemented twice, with no shared code enforcing agreement

`crates/dispatcher/src/guards.rs` (the pre-flight guard: capability check → consequence gate →
zone-write-class gate → magnitude gate) and `crates/executor/src/risk_gated.rs`
(`RiskGatedToolRuntime::invoke`, the runtime-level guard for adaptive/non-seed tool calls) both
implement the same four-step sequence independently — not calling shared logic, two parallel
implementations that happen to agree today. Nothing (compiler or otherwise) stops a future guard
addition from landing in one and being missed in the other, silently weakening the runtime safety
net for adaptive calls while the pre-flight guard still looks complete. This is the single
highest-leverage decomposition in the codebase given what's actually at stake if it drifts. Fix:
extract a shared `evaluate_call(...)` (in `liberado-common` or a new `liberado-guards` crate) that
both call; the leaf helpers (`is_sweeping_destructive`, `resolve_zone`, `mcp_of`) are already shared,
only the sequencing logic needs unifying.

**Resolution (2026-07-05):** Extracted the zone-write-class check — the most complex of the four,
and the one with real independently-written logic at each site — into
`liberado_common::zone_write_restriction(mcp_name, tool_name, zone_catalog, zone_write_classes) ->
Option<String>` (`crates/common/src/catalog.rs`, next to the `resolve_zone` it builds on), with 5
new unit tests covering restricted/allowed/unlisted-fails-safe/untracked-MCP/untracked-tool. Both
`guards.rs::zone_restricted` and `risk_gated.rs::invoke`'s zone check now call this one function
instead of independently re-implementing it — this determination can no longer drift between the
two enforcement points.

The capability, consequence, and magnitude checks were deliberately **not** forced into the same
unification: they operate over genuinely different shapes at each site (the dispatcher's guard
checks a decision's *declared* seed calls before anything runs; the runtime guard checks one *live*
call with real arguments), and forcing a shared signature there would mean widening one side's data
model to match the other for a check that's only 2-3 lines and low-risk to keep separate. Instead,
added explicit cross-referencing doc comments at both `guards.rs`'s module doc and
`risk_gated.rs`'s module doc: "if you add a new guard here, check whether the other file needs the
equivalent" — this doesn't close the drift risk at the type level for those three checks, but makes
it visible to the next person who touches either file, which the codebase had nothing of before.
Also fixed the `provenanace` typo the audit flagged in passing (`orchestrator/src/lib.rs:264`).

Verified: `cargo test -p liberado-common -p liberado-dispatcher -p liberado-executor -p
liberado-orchestrator` all green (no behavior change, same guard outcomes for every existing test),
plus a full `cargo build --workspace` / `cargo test --workspace` with zero regressions.

### `GET /api/conversations/{id}` breaks its own wire contract

`crates/server/src/api.rs:283` serializes `Vec<liberado_provider::Message>` (an internal type, not a
wire DTO) through a hand-rolled `serde_json::json!({"messages": messages})` literal, while
`chat-client-contract` exists specifically so every response has one canonical typed shape all three
clients agree on. TUI and webui both deserialize this expecting `ChatMessage`. If the two types ever
diverge in field name or shape, clients get silently wrong data with no compiler check. Fix: add
`ConversationHistoryResponse { messages: Vec<ChatMessage> }` to `chat-client-contract::wire` and use
it here — the one place the wire contract is actually breached today.

**Resolution (2026-07-05):** Added `ConversationHistoryResponse { messages: Vec<ChatMessage> }` to
`chat-client-contract::wire` (with a roundtrip test) and a `chat_message_from_provider` conversion
function in `crates/server/src/api.rs` — the single place `liberado_provider::Message` (internal:
`Role` enum, `Vec<ToolInvocation>`) turns into the wire `ChatMessage` (`role: String`,
`tool_calls: Option<Value>`), reusing the same `Role`-to-string match already established in
`liberado-provider`'s `openai_compat.rs`. `get_conversation` now returns
`Json(ConversationHistoryResponse { messages })` instead of the hand-rolled `json!(...)` literal.

While in there: found the TUI had its own private `ConversationHistory { messages: Vec<ChatMessage> }`
struct in `crates/tui/src/api.rs` — an exact duplicate of the wrapper type this fix just added
upstream. Deleted it and switched the TUI to the shared `ConversationHistoryResponse`. Also
simplified webui's `fetch_conversation` (`crates/webui/src/components/chat.rs`), which was manually
digging `json.get("messages")` out of an untyped `serde_json::Value` — now deserializes directly into
the shared type. The wire *shape* (`{"messages": [...]}`) never changed, so this was pure
consolidation onto the canonical type, not a behavior change for either client.

Verified: `cargo test -p chat-client-contract -p liberado-server -p liberado-tui` all green (55/12/217
tests respectively), `dx build --package liberado-webui --web` clean, plus a full
`cargo build --workspace` / `cargo test --workspace` with zero regressions.

### `mcp-forge`'s unpinned-`rev` sources silently track a moving upstream target

`crates/mcp-forge/src/build.rs:43-110`: `git ls-remote` resolves a SHA, compared against the
lockfile, then `cargo install --git <url>` re-resolves the ref independently — a TOCTOU gap in
principle, but the real issue is `McpSource.rev: Option<String>`, where `None` (line 44) falls back
to tracking the live branch HEAD. Every `sync` of a `rev`-less source silently rebuilds against
whatever the upstream just pushed, with no local review step, for a daemon that then runs that code.
Fix: require `rev` for any source used outside active development (validate at config-load time), or
at minimum pass the resolved SHA as `--rev` to `cargo install` so both steps agree on one commit.

**Resolution (2026-07-05):** Took the "at minimum" option — `cargo_install` now takes the already-
resolved SHA as an explicit parameter and always passes it as `--rev <sha>`, regardless of whether
`source.rev` was a branch name, a tag, or absent (in which case `resolve_remote_sha` had resolved
`HEAD`). This closes the TOCTOU gap completely: whatever `cargo install` builds is now guaranteed to
be exactly the commit `resolve_remote_sha` resolved and that `sync_source` records into the lockfile
afterward. `rev`-less sources still track a moving branch tip across separate `sync` runs (that part
is unchanged and arguably intentional for active development), but within a single sync there's no
longer a window for upstream to push a different commit than the one that gets built and recorded.
Left the "require `rev` in production" option on the table — doing so would need a new
production/development distinction that doesn't exist anywhere else in the config model today, and
the TOCTOU fix already removes the actual silent-drift hazard the finding was about.

Verified: `cargo build -p liberado-mcp-forge` clean, `cargo test -p liberado-mcp-forge` (6/6 passing,
unchanged — none of these exercise `cargo_install`/`sync_source` directly since they shell out to
real `git`/`cargo`, consistent with the existing test suite's scope).

### `topology.provider` is a config field that lies

`crates/bootstrap/src/lib.rs:34-46`: `provider_from_env()` always constructs a `DeepSeekProvider`
regardless of `config.topology.provider`'s value — that field is accepted, defaults to `"deepseek"`,
and is never read. Setting `topology.provider = "openrouter"` does nothing, silently. Fix: either wire
the field to actually select a provider, or remove it and document provider selection as
environment-variable-only (`liberado-provider-openrouter` already exists as a real alternative, so
this isn't hypothetical).

**Resolution (2026-07-05):** Wired the field up rather than removing it. `provider_from_env()` is now
`provider_from_config(config: &Config)`, matching on `config.topology.provider.as_str()`:
`"openrouter"` builds an `OpenRouterProvider::from_env()`, `"deepseek"` (the default) and any
unrecognized value build the `DeepSeekProvider` as before — an unrecognized value now also logs a
`tracing::warn!` naming the bad value, instead of silently doing the same thing as before with no
signal at all. Added `liberado-provider-openrouter` as a `liberado-bootstrap` dependency (it was
already a workspace member/dependency elsewhere, just not wired to this crate). Updated the one
caller (`crates/server/src/lib.rs:73`) to pass `&config` through. Added two tests following this
codebase's existing convention for `from_env`-backed tests (`provider-deepseek`/`provider-openrouter`'s
own `from_env_uses_environment_variables` tests) of asserting against whatever the process's real env
happens to be, rather than mutating env vars (which would race under parallel test execution):
`unknown_provider_name_falls_back_to_deepseek_selection` and
`openrouter_provider_name_routes_to_openrouter`.

Verified: `cargo test -p liberado-bootstrap` (8/8, was 6), `cargo test -p liberado-server` (12/12),
both crates build clean.

## Priority 2 — real, worth doing when nearby

- **`run_loop`'s `loop_strikes` counter is shared between two independent escalation ladders**
  (`crates/executor/src/lib.rs:607-815`) — doom-loop detection and short-cycle detection both consume
  the same strike counter, so a nudge from one mechanism can exhaust the budget before the other gets
  its own second chance. This reads like it could already be a live behavioral bug, not just an
  entanglement risk — worth a dedicated look, not just a refactor. The surrounding 200+-line method
  (turn counting, resource limits, prose-nudge, doom-loop, cycle detection, dispatch) is a
  decomposition candidate regardless (`LoopGuard` owning `call_history`/`loop_strikes` with named
  methods per step).

  **Resolution (2026-07-05):** Confirmed it was a live bug, not just entanglement risk: with one
  shared counter, whichever mechanism detected a problem *second* silently inherited the other's
  strike count, so its own first-ever detection could jump straight to tool removal instead of
  nudging first (e.g. a short cycle nudging once, then an entirely unrelated doom loop immediately
  removing a tool it had never itself nudged for). Fixed by adding a small `LoopGuard` struct
  (`strikes: u8` + a `strike() -> Escalation` method returning `Nudge`/`Remove`/`GiveUp`) and giving
  `run_loop` two independent instances — `doom_guard`/`cycle_guard` — instead of one shared
  `loop_strikes: u8`. Did *not* extract the larger decomposition suggested above (pulling
  `call_history`/turn-counting/dispatch into `LoopGuard` too) — the actual bug was the shared
  counter, not the method's length, and `run_loop` already reads as one coherent state machine;
  splitting it further wasn't justified by this fix alone. The one-time turn-budget top-up
  (`bonus_granted`) correctly stays a single flag shared by both guards, since that grant is
  genuinely per-run, not per-mechanism. Added a regression test,
  `a_doom_loop_gets_its_own_nudge_even_after_the_cycle_guard_already_struck_once`, that nudges the
  cycle guard once, breaks the cycle pattern, then drives a first-ever doom-loop detection and
  asserts it still nudges (`DOOM_LOOP_NUDGE` sent) rather than skipping straight to removal — with
  the old shared counter, the doom-loop detection would inherit strike count 2 from the cycle
  guard's prior nudge and remove `search` immediately, so `DOOM_LOOP_NUDGE` would never be sent and
  this test would fail against the pre-fix code.
  Verified: `cargo test -p liberado-executor` (32/32, up from 31 with the new test).
- **`complete_stream`'s SSE loop is duplicated verbatim** between `provider-deepseek/src/lib.rs:104-179`
  and `provider-openrouter/src/lib.rs:117-193` — the `openai_compat` module already extracted the
  mapping functions but left the harder part (the loop that drives them, where chunk-boundary bugs
  hide) duplicated. Extract `openai_compat::stream_sse_response(...)`.

  **Resolution (2026-07-05):** Added `pub fn stream_sse_response(response: reqwest::Response,
  name_map: ToolNameMap) -> CompletionStream` to `crates/provider/src/openai_compat.rs` — the exact
  loop body moved verbatim from both call sites, taking over from the point right after each
  provider's own POST + status-code check. Both `complete_stream` implementations now end with a
  single `Ok(stream_sse_response(response, name_map))`. This required adding `reqwest` and
  `async-stream` to `liberado-provider`'s own `Cargo.toml` (both already workspace-level deps, just
  not previously needed by this crate); removed the same two from `provider-deepseek`/
  `provider-openrouter`'s `Cargo.toml`s since neither uses them directly anymore. Did not add a new
  unit test for the streaming loop itself — neither original call site had one before this fix
  either (both only tested `map_status`/constructor/`endpoint`/`from_env`/`model_getter`), and
  testing it properly would need a `reqwest::Response` built from a fake HTTP body (no existing
  mocking infra in this workspace for that), which is more test-infrastructure work than this
  dedup-only fix justifies on its own; the chunk-assembly logic remains exercised the same way it
  always was, indirectly, via live use.
  Verified: `cargo test -p liberado-provider -p liberado-provider-deepseek
  -p liberado-provider-openrouter` (14/7/7 passing, all pre-existing — no test changes), full
  `cargo build --workspace` clean.
- **`PROPOSALS_DIR = "proposals"` is declared independently in three crates**
  (`daemon`, `telegram-approvals`, `executor/risk_gated.rs`) with a comment in one noting it "matches"
  the others — a comment, not a shared constant. Export one `PROPOSALS_DIR` from `liberado-common`.

  **Resolution (2026-07-05):** Added `pub const PROPOSALS_DIR: &str = "proposals"` to
  `crates/common/src/proposal.rs` (the existing home of the `Proposal` type itself) and re-exported
  it from `liberado-common`'s crate root. `daemon`/`telegram-approvals` each deleted their own private
  `const PROPOSALS_DIR` and now import the shared one; `executor/risk_gated.rs` (which had never
  named it as a constant at all, just a bare `.join("proposals")` literal) now does
  `self.proposals_dir.join(liberado_common::PROPOSALS_DIR)`. Left the test-fixture literal at
  `risk_gated.rs`'s `a_proposal_write_failure_is_a_real_error_not_a_silent_ok`-adjacent test
  (`dir.path().join("proposals")`, verifying what the runtime actually wrote) as a literal rather than
  swapping in the constant there too — it's asserting against the real on-disk convention from the
  outside, not re-declaring it, so a literal there is the more honest test.
  Verified: `cargo test -p liberado-common -p liberado-daemon -p liberado-telegram-approvals
  -p liberado-executor` (25/17/12/32 passing), `cargo build` clean for all four.
- **`chat-search`'s local `Record` enum silently mirrors `conversation-store`'s private one**
  (`crates/chat-search/src/scan.rs:14-20`, already documented as intentional in this session's own
  work) — a future new `Record` variant upstream would be silently skipped by search's
  `Err(_) => continue` path rather than erroring, meaning search would quietly stop reflecting new
  message types. Export `Record` from `conversation-store` (even as `pub(crate)` + a re-export) so
  this becomes a real dependency instead of a mirrored guess.

  **Resolution (2026-07-05):** Changed `enum Record` in `crates/conversation-store/src/jsonl.rs`
  from private to `pub` and re-exported it from the crate root (`pub use jsonl::{JsonlStore,
  Record}`). Deleted `chat-search/src/scan.rs`'s private mirror entirely and imported the real type
  (`use liberado_conversation_store::{Author, ConversationHeader, Record}`) — a future new `Record`
  variant now shows up as a real compile error in `scan.rs`'s `match record { ... }` (non-exhaustive
  match) instead of silently deserializing as a parse failure that the best-effort `Err(_) =>
  continue` path swallows.
  Verified: `cargo test -p liberado-conversation-store -p liberado-chat-search` (9/15 passing, no
  test changes needed — the wire shape didn't change, only where the type is defined), full `cargo
  build --workspace` clean.
- **`ChatClient` trait in `chat_client_contract::native` still has zero implementations** — confirmed
  still true (this was previously flagged as a known gap). TUI and CLI each hand-roll their own
  `turn()`/stream-request plumbing instead of sharing one implementation. Either implement it in one
  shared struct both clients use, or delete the trait and document `SseDecoder` +
  `ChatEvent::from_sse_data` as the actual (real, working) shared boundary.

  **Resolution (2026-07-05):** Deleted the trait — checked both real clients first
  (`crates/cli/src/chat_client.rs`, `crates/tui/src/effects.rs`) and confirmed their actual needs
  diverge past what a `send`/`stream` trait usefully captures: the CLI drives a blocking terminal
  REPL loop; the TUI feeds a non-blocking render loop through its own action/effect channel
  architecture. Forcing a shared struct implementing `ChatClient` would mean bending one of the two
  (most likely the TUI's effect system) around an abstraction neither had ever actually reached for
  in however long the trait sat unused. Documented the real, currently-used shared boundary instead,
  in both `lib.rs`'s module doc and a new doc comment on `native.rs` itself: `SseDecoder` (SSE
  framing) is genuinely shared by both clients today; `ChatEvent::from_sse_data` (typed payload
  decoding) is used on top of it by the TUI's `sse::ToAction`, but the CLI still parses its own
  `tool`/`tool_result` JSON payloads inline — noted as a smaller, separate, optional follow-up (not
  folded into this fix, to avoid scope creep into changing a working client's parsing for a
  code-cleanliness reason alone). Removing the trait let `async-trait`/`tokio`/`futures`/`ulid` come
  out of `chat-client-contract`'s `Cargo.toml` entirely — none of `native.rs`'s remaining
  `SseDecoder` code needed them; they existed only for the now-deleted trait.
  Verified: `cargo test -p chat-client-contract -p liberado-tui` (55/217 passing), `cargo build -p
  liberado-cli` clean, full `cargo build --workspace` clean.
- **`Orchestrator::new` takes 9 positional parameters**, 7 identical across every pool
  (`crates/orchestrator/src/lib.rs:138-163`, called from `crates/bootstrap/src/lib.rs:172-182,224-234`)
  — already has `#[allow(clippy::too_many_arguments)]`, a sign this was already noticed. A builder
  separating per-pool-varying fields from shared infrastructure would make a future new parameter a
  compile error at every call site instead of a silent gap.

  **Resolution (2026-07-05):** Added `OrchestratorInfra` (`crates/orchestrator/src/lib.rs`) bundling
  the 6 fields that don't vary per pool (`provider`, `consequence_catalog`, `zone_catalog`,
  `zone_write_classes`, `proposals_dir`, `signer`), with a `for_pool(factory, capabilities,
  pool_name)` method that combines it with the 3 that do. `crates/bootstrap/src/lib.rs`'s
  `configure_daemon` now builds one `OrchestratorInfra` right after `guard_context(...)` and calls
  `.for_pool(...)` at both its call sites (the default pool and the per-named-pool `fold`), instead
  of re-cloning all 6 shared values into a 9-argument `Orchestrator::new` twice. Left
  `Orchestrator::new` itself in place, unchanged — deliberately did not touch its ~30 other call
  sites (orchestrator's own unit tests, `daemon`/`main-agent`/`telegram-approvals`'s test fixtures),
  which construct `Orchestrator` directly because each test wants full, independent control over
  every field for its own fixture; forcing all of them onto `OrchestratorInfra` for a code-cleanliness
  reason alone would be a large, risk-bearing test-file rewrite unrelated to the actual finding, which
  was specifically about the two *production* call sites in `configure_daemon` re-cloning the same 6
  values.
  Verified: `cargo test -p liberado-orchestrator -p liberado-bootstrap` (7 + 15 + 8 passing, no
  existing test needed changes since `Orchestrator::new` itself didn't change), full `cargo build
  --workspace` clean.
- **No type-level enforcement that a `Proposal` is signed before being written** — `handle_proposal_change`
  (`crates/daemon/src/lib.rs:469-476`) correctly rejects unsigned proposals at verify-time, but nothing
  stops a proposal-creation site from forgetting to call `signer.sign()` first; the only failure mode
  is proposals that silently become permanently non-executable. A `SignedProposal` newtype
  constructable only via `ProposalSigner::sign` closes this at the type level.

  **Resolution (2026-07-05):** Added `SignedProposal(Proposal)` (`crates/common/src/proposal.rs`) —
  a tuple struct with a private field, `Deref<Target = Proposal>` for read access, `as_proposal`/
  `into_proposal` escape hatches, and a narrow `set_status` (the one field `ProposalSigner::compute`
  deliberately excludes from the signature, so it's safe to change post-signing without
  invalidating it — everything else stays immutable through the type, no `DerefMut`, since allowing
  that would let a caller mutate a signed field and silently break the signature, the exact bug class
  this exists to prevent). `ProposalSigner::sign` changed from `sign(&self, proposal: &mut Proposal)`
  (mutate in place) to `sign(&self, proposal: Proposal) -> SignedProposal` (consume, return the
  wrapped guarantee) — the only way to produce a `SignedProposal` at all. Every proposal-writing
  helper now takes `&SignedProposal` instead of `&Proposal`: `Disposition::Propose` (`orchestrator`),
  `Daemon::write_proposal`, `ChatSessions::write_chat_proposal`, and
  `RiskGatedToolRuntime::write_proposal`'s internal build-then-write — so a future call site that
  forgot to sign fails to compile instead of writing a proposal that only fails verification later,
  at approval time. `execute_approved` (which re-verifies a proposal read back from an
  already-written, human-editable note) deliberately keeps taking a plain `&Proposal` — that boundary
  is genuinely re-establishing trust from untrusted disk content, not the creation-time guarantee
  `SignedProposal` is about.

  This touched every call site of `.sign(...)` (18 of them, mostly tests): each `signer.sign(&mut
  proposal)` became `let proposal = signer.sign(proposal)` (or `.into_proposal()` where a test needed
  to mutate a field afterward to test tamper-detection, e.g. `tampered_pool_fails_verification`).
  While updating `ChatSessions::write_chat_proposal`'s signature, noticed and fixed a stray
  hard-coded `"proposals"` literal there too (should have been `PROPOSALS_DIR` from the P2.3 fix
  above, but `main-agent` wasn't one of the three crates that finding named) — a small drive-by,
  not a separate investigation.

  Verified: `cargo test -p liberado-common -p liberado-orchestrator -p liberado-executor -p
  liberado-daemon -p liberado-telegram-approvals -p liberado-main-agent` (38+25+32+17+12+15 = 139
  passing, 0 failed), then a full `cargo build --workspace` + `cargo test --workspace` with zero
  regressions anywhere in the tree.
- **`with_zone_write_classes` is a silent no-op if called before `with_dispatcher`**
  (`crates/daemon/src/lib.rs:281-287`) — a misordered bootstrap call would silently under-enforce zone
  restrictions with no error, for a security-relevant setting. Current call order happens to be
  correct; nothing enforces it stays that way.

  **Resolution (2026-07-05):** Took the "restructure" option over "error/panic on misorder" — folded
  `zone_write_classes: Vec<(String, WriteClass)>` directly into `Daemon::with_dispatcher`'s own
  parameters and deleted the separate `with_zone_write_classes` builder method entirely, so there is
  no second call left to get the order of wrong. `with_pool_dispatcher` (the named-additional-pool
  variant) deliberately did **not** get the same parameter — v1 additional pools still have no
  zone-write-class configuration of their own (unchanged pre-existing scope, documented inline now
  rather than left implicit), so this fix is purely about closing the ordering hazard for the
  default pool, not about extending per-pool zone config. Updated the one production caller
  (`crates/bootstrap/src/lib.rs`'s `configure_daemon`) to pass `guard.zone_write_classes.clone()`
  straight into `.with_dispatcher(...)` instead of a trailing `.with_zone_write_classes(...)`, and
  the 4 test call sites in `crates/daemon/src/lib.rs` to pass `Vec::new()` (none of them were
  exercising zone-write-class behavior anyway — see P3.4 below for the still-missing end-to-end
  test of that guard).
  Verified: `cargo test -p liberado-daemon -p liberado-bootstrap` (17/8 passing), full `cargo build
  --workspace` clean.
- **`JsonlStore::set_title` rewrites the entire conversation file in place**
  (`crates/conversation-store/src/jsonl.rs:302-327`) — correct today, but a crash mid-write truncates
  the whole conversation, and `list()` reads full files just to parse the header line. Not urgent at
  personal-scale corpora, but `set_title`'s all-or-nothing rewrite is a real correctness hazard
  (sidecar file or write-then-rename would remove it); `list()`'s full-read is a cheap short-circuit
  fix whenever noticed.

  **Resolution (2026-07-05):** `set_title` now writes the rebuilt contents to a sibling
  `<id>.jsonl.tmp` file, then `tokio::fs::rename`s it over the real path — a crash between the two
  leaves either the untouched original or the fully-written new content, never a truncated file.
  Confirmed `rename` replaces an existing destination on Windows too (`std::fs::rename` there uses
  `MOVEFILE_REPLACE_EXISTING`, same semantics as POSIX `rename(2)`), and staying in the same
  directory keeps both paths on one filesystem, which is what makes the rename atomic rather than a
  copy. `list()` no longer reads each file fully — opens it and reads only line 0 via
  `tokio::io::BufReader`/`AsyncBufReadExt::lines()` (the same `.lines()`/`.next_line()` pattern
  already used in `crates/cli/src/chat_client.rs`), instead of `tokio::fs::read_to_string` followed
  by throwing away everything past the first line.

  `set_title` had zero test coverage before this — added
  `set_title_updates_the_header_and_preserves_every_node` (title changes, both existing nodes and
  their parent-child link survive the rewrite) and `set_title_leaves_no_temp_file_behind` (confirms
  the `.tmp` file actually gets renamed away, not left orphaned). Did not attempt to test the actual
  crash-mid-write scenario itself (no clean way to interrupt a `tokio::fs::write` mid-flight in a
  unit test); the atomicity guarantee here rests on `rename`'s documented semantics, not a test that
  simulates a crash.

  Verified: `cargo test -p liberado-conversation-store` (11/11, up from 9), `cargo test -p
  liberado-chat-search` (15/15, unaffected — it reads `.jsonl` files directly, doesn't go through
  `list()`/`set_title`), full `cargo build --workspace` clean.
- **`chat-search`'s literal AND mode matches within one message, not across a conversation** — a
  search for "auth token" won't find a conversation where "auth" and "token" appear in different
  messages. Documented in the code as v1 scope, but neither the REST endpoint's doc comment nor the
  MCP tool description surfaces this to a user who will naturally expect conversation-level matching.
  Worth a one-line doc fix now; an OR-across-messages mode later if it's actually felt.

  **Resolution (2026-07-05):** Added the missing sentence to both surfaces named in the finding.
  `crates/server/src/api.rs`'s `search_conversations` doc comment now says explicitly that ALL terms
  must appear in **the same message**, not just anywhere in the conversation, with the `"auth
  token"` example spelled out. `crates/chat-search-mcp/src/main.rs`'s `#[tool(description = "...")]`
  string — the text actually surfaced to a model deciding whether/how to call the tool, not just the
  doc comment above it — got the same clarification ("per-message, not per-conversation... terms
  split across different messages won't match"), since that's the copy a dispatcher reasoning about
  whether this tool can answer a given query actually reads. Doc-only change, no behavior touched.
  Verified: `cargo test -p liberado-server` (12/12), `cargo build -p liberado-chat-search-mcp`
  clean, full `cargo build --workspace` clean.

## Priority 3 — low, fix opportunistically

- `dispatch_parallel` builds its own `Executor` inline instead of going through the orchestrator's own
  `self.execute` helper (`crates/orchestrator/src/lib.rs:534-537`) — correct today, bypasses the
  canonical construction path if it's ever extended.

  **Resolution (2026-07-05):** `self.execute`'s body needs `&self` (only to reach `self.provider`),
  which `dispatch_parallel`'s spawned `tokio::spawn` tasks can't hold (they must own everything they
  capture, since they outlive this call's borrow of `self`) — that's the actual reason it had its
  own inline `Executor::new(...)` rather than a copy-paste oversight. Fixed by splitting the shared
  logic into a new `Orchestrator::execute_with(provider: Arc<dyn Provider>, budget: &Budget, runtime:
  &dyn ToolRuntime, task: Task)` associated function that takes an **owned** provider instead of
  `&self` — `self.execute` is now a one-line wrapper calling `Self::execute_with(self.provider.clone(),
  ...)`, and `dispatch_parallel`'s spawned closure calls the same `Self::execute_with(provider, ...)`
  with its own already-cloned `provider`. One real construction path, used by both callers, neither
  of which needs to hold `&self` across the actual execution.
  Verified: `cargo test -p liberado-orchestrator` (7 + 15 passing, unchanged), full `cargo build
  --workspace` clean.
- `configure_daemon` calls `mcp_registry_from_config` once per pool (N+1 total for N extra pools) —
  cheap today, undocumented as intentional at the call site.

  **Resolution (2026-07-05):** Doc-only fix, no behavior change — added an inline comment directly
  at the per-pool `mcp_registry_from_config(config)` call site inside `configure_daemon`'s `fold`
  (`crates/bootstrap/src/lib.rs`) spelling out why this is intentional: each pool needs its own,
  independently owned `McpRegistry` (they aren't `Clone`/shareable across orchestrators), so there's
  no cheaper way to produce N separate registries than building each from the same in-memory
  `config.topology.mcps` N times — and since that's a small in-memory config, not a file or network
  round-trip, it's genuinely cheap and not worth caching or restructuring around.
  Verified: `cargo build -p liberado-bootstrap` clean (doc comment only), `cargo test -p
  liberado-bootstrap` (8/8, unchanged).
- Two config fields (`ambient_sweep_schedule`, `git_commit_schedule`,
  `crates/config-loader/src/model.rs:536-573`) are stringly-typed with no validation, at odds with the
  project's own fail-fast philosophy elsewhere.

  **Resolution (2026-07-05):** Found a third field in the same situation while investigating —
  `MaintenanceTuning::maintenance_schedule` — and fixed all three the same way. First checked why
  `topology.schedules[].cron_expr` gets real cron-parser validation but these three don't: it's
  because `cron_expr` is actually consumed (`liberado_cron::CronEventSource` parses it), while
  grepping the whole workspace for `ambient_sweep_schedule`/`git_commit_schedule`/
  `maintenance_schedule` turned up **zero** consumers of any of the three — the ambient-sweep and
  git-maintenance features these describe (per `liberado-inbox-spec.md` §11 and
  `liberado-vault-maintenance-and-git-spec.md` §5) aren't built yet, so no concrete schedule syntax
  has actually been decided. Adding strict cron-style parsing now would lock in a format ahead of the
  feature that needs it — the kind of speculative validation the codebase's own conventions avoid.
  Settled on the honest middle ground: reject an empty/whitespace-only value in `Config::validate`
  (unambiguously wrong under any future interpretation) and add a doc comment on each field stating
  plainly that it's free text with no consumer yet, and that whichever component eventually reads it
  should pick a real syntax and add proper parse validation at that point.
  Added `blank_schedule_fields_fail_validation` covering all three plus a defaults-still-pass
  assertion. Verified: `cargo test -p liberado-config-loader` (59/59, up from 58), full `cargo build
  --workspace` clean.
- No daemon-level integration test exercises the zone-write-class guard end-to-end (unit-tested in
  `dispatcher`/`executor` separately, but not proven to agree at the daemon level) — the most likely
  place a Priority 1 guard-duplication drift would hide undetected.

  **Resolution (2026-07-05):** Added `daemon_downgrades_a_zone_restricted_seed_call_to_a_proposal`
  in `crates/daemon/src/lib.rs`, modeled directly on the existing
  `daemon_emits_a_proposal_for_a_high_consequence_action` test (same `UnusedFactory`/`UnusedRuntime`
  pattern — the orchestrator's `Propose` arm never touches the runtime factory). Registers an MCP
  with `consequence: Consequence::Reversible` (deliberately *below* the consequence gate, which
  triggers at `Irreversible`) and `default_zone: Some("reviews")`, configures
  `zone_write_classes: [("reviews", WriteClass::ProposalOnly)]` via `with_dispatcher`'s 4th
  parameter (the P2.8 fix), writes a note that gets classified into an `ExecuteDirect` seed call
  against that MCP, and asserts the daemon emits a real, signed `Propose` disposition whose written
  note round-trips correctly — proving the configured zone restriction, not the consequence gate, is
  what's actually blocking direct execution end-to-end through the real `Daemon`/`Dispatcher`
  machinery, not just the `guards::evaluate`/`RiskGatedToolRuntime` unit tests in isolation.
  Verified the test is real, not vacuous, before finalizing: temporarily changed the configured
  class to `WriteClass::AgentWritable` and confirmed the test fails (`expected Acted/Propose, got
  ExecuteDirect`), then reverted.
  Verified: `cargo test -p liberado-daemon` (18/18, up from 17), full `cargo build --workspace`
  clean.

## What's genuinely fine (worth stating, not just omitting)

- Capability narrowing (`CapabilitySet::narrow` as intersection-only), the consequence/magnitude
  ordering, the `ProposalSigner` HMAC mechanism and its test suite, and the loop-break provenance model
  are all well-structured — the guard *logic* itself is correct and well-tested at the unit level; the
  Priority 1 finding above is about *duplication*, not incorrectness.
- The `Provider` trait is a clean seam — `DeepSeekProvider`/`OpenRouterProvider` don't leak into any
  caller, and the `openai_compat` module already did the hard part of de-duplicating the mapping logic.
- Config validation (`Config::validate`/`validate_merged_config`) has solid fail-fast coverage: vault
  path, cron expressions, duplicate names, dangling zone/MCP references, missing secrets — each tested.
- Cross-client sharing is *mostly* working, contrary to what Priority 2's `ChatClient` finding might
  suggest in isolation: all three clients correctly use `SseDecoder`/`ChatEvent::from_sse_data` and
  `liberado-commands`' `CommandContext` for slash commands. No client has quietly re-implemented its
  own SSE parser — the gap is narrower (one dead trait, one wire-contract breach) than "the clients
  have drifted apart."
- `liberado-cron`, the `liberado-notify`/`liberado-telegram-approvals` Approve/Revise safety split, and
  `chat-search`'s own test coverage (happy path, limits, regex mode, empty-directory) are all solid.
