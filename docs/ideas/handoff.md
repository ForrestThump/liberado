# Liberado — Handoff (2026-07-02)

Current-state handoff. Kept in sync by the Dream skill after each session arc. For the authoritative
system map read [`docs/architecture/overview.md`](../architecture/overview.md); for build/run/configure
read [`docs/contributing/agents.md`](../contributing/agents.md); for the development process (how to
research, plan, delegate, test, commit) read
[`docs/contributing/development-workflow.md`](../contributing/development-workflow.md); for the chat
API contract read [`docs/reference/api.md`](../reference/api.md); for the rationale behind any
"Decision N" read [`docs/specs/liberado-architecture-decisions.md`](../specs/liberado-architecture-decisions.md).

> Note: keep this file lowercase (`handoff.md`, not `HANDOFF.md`). On Windows (case-insensitive
> filesystem) the two names collide — see Pitfalls below.

---

## What's built (as of 2026-07-02)

All fourteen "Done" milestones in [`docs/architecture/overview.md`](../architecture/overview.md) are
shipped and `cargo test --workspace` is green. The system is:

- **One `liberado` binary** (daemon-first, Decision 2): `liberado serve [vault]` runs the daemon + chat
  + HTTP/SSE API; `liberado chat [session]` is a native client; bare `liberado <vault>` aliases `serve`.
- **Phases 1 and 2 complete**: chat routes through the dispatcher; capability catalog is live and shared;
  `crates/tui` is a ratatui client; web UI has sidebar/MCP panel/Markdown/slash commands; Riggers
  (`code-dispatch`) enables self-improvement via draft PRs.
- **Proposal loop hardened (2026-07-02)**: HMAC-SHA256 integrity signing on every proposal (item 2 of
  hardening audit); runtime-gated proposals now land in the vault's own `proposals/` dir so approve→execute
  works for adaptive-call downgrades too (item 3). Item 1 (writer-identity verification) remains open —
  needs OS-level MCP sandboxing or an out-of-band approval channel.
- **Crate hygiene passes (2026-07-01 to 2026-07-02)**: three tiers — test-mock dedup, `RuntimeFactory`
  relocation to `liberado-executor`, new `liberado-config` crate extracted from `liberado-bootstrap`. Full
  record: [`docs/roadmap/hygiene-audit-2026-07-02.md`](../roadmap/hygiene-audit-2026-07-02.md).

**Not yet built (next slice)**: inbox hook, hooks generally, multi-MCP registry UX, connection pooling.
`ChatClient` trait adoption (crate-modularity-audit finding 2) and splitting `liberado-common`'s
nine-module grab-bag (finding 3) are the primary crate-structure deferred items. Phases 3 and 4 are on
the roadmap. See [`docs/roadmap/current.md`](../roadmap/current.md) for the full list.

---

## Where key things live

| What | Where |
|---|---|
| Shared type vocabulary | `crates/common/src/` |
| Proposal type + signer | `crates/common/src/proposal.rs` |
| Per-installation proposal signing key | `crates/config/src/lib.rs` (`load_or_create_proposal_key`), stored at `<data_dir>/.proposal-key` |
| Dispatcher (classify + guards) | `crates/dispatcher/src/` |
| Executor (agent loop) | `crates/executor/src/` — `RiskGatedToolRuntime` lives here |
| Orchestrator (bridges decision → execution) | `crates/orchestrator/src/lib.rs` |
| Multi-turn conversation (chat loop) | `crates/main-agent/src/sessions.rs` (`ChatSessions`) |
| Daemon watch loop | `crates/daemon/src/lib.rs` — `handle_proposal_change` routes approved proposals |
| Shared env wiring | `crates/bootstrap/src/lib.rs` |
| Config loading + `GuardContext` | `crates/config/src/lib.rs` |
| Binary entry + arg dispatch | `crates/cli/src/main.rs` |
| Chat client (CLI) | `crates/cli/src/chat_client.rs` |
| Server library | `crates/server/src/lib.rs` — `run()` is the daemon entry point |
| TUI client | `crates/tui/src/` |
| Web UI (Dioxus WASM) | `crates/webui/src/` — `dx build` only, excluded from native workspace build |
| Conversation store | `crates/conversation-store/src/{jsonl,store,types,error}.rs` |
| Shared SSE decoder + slash-command dispatcher | `crates/chat-client-contract/` and `crates/liberado-commands/` |

---

## Working patterns that succeed (keep doing)

- **Research before design — verify claims against code.** Read the struct/function/route before trusting
  a doc that describes it. Checking the actual `Cargo.toml` and `wc -l` caught at least one hygiene
  finding this session that turned out to be **false on both counts** — a plausible-sounding claim the
  premise check ruled out immediately.
- **Dispatch 3 parallel research subagents for audit-shaped work**, one per independent angle (coupling /
  duplication / dead code; or guard coverage / integrity / injection surfaces). Each agent gets the full
  architecture context already known, a narrow angle, a demand for file:line verdicts, and a word budget
  (~600-700 words). Three complementary deep reports, not three overlapping shallow ones.
- **Verify code staleness claims against code, not narrative** — the docs review pass that found
  `api.md` missing two routes (`GET /api/catalog`, `PATCH /api/conversations/{id}`) was only reliable
  because the claim was checked against the actual `crates/server/src/api.rs` route table, not accepted
  from a summary.
- **Cheap link-checking catches bugs human review misses.** Walking `docs/`, regex-extracting markdown
  links, and verifying each target path caught 22 broken links in a single pass — several of which
  predated this session's own edits.
- **Dispatch a subagent for code, then verify independently**: read the produced code, run `cargo build`
  then `cargo test --workspace --no-run` (they're different — the second compiles `#[cfg(test)]`), then
  `cargo test --workspace`. Don't trust the subagent's own report.
- **Live smoke recipe** (proven repeatedly): hydrate the key from the Windows User env via
  `[Environment]::GetEnvironmentVariable("DEEPSEEK_API_KEY","User")` (NEVER print it; only confirm
  length 35 / prefix `sk-`); start `liberado serve <scratch-vault>` on a scratch `LIBERADO_PORT` +
  `LIBERADO_DATA_DIR`; drive a two-turn message through `liberado chat`; assert continuity (turn 2
  recalls a word from turn 1) and a persisted ULID `.jsonl` under `<LIBERADO_DATA_DIR>/conversations`.
- A force-killed server (`Stop-Process -Force`) exits **255** — expected, not a failure.
- **Split audit into two commits**: first the findings doc (`docs/roadmap/`), then the fix. Each is
  individually reviewable and the findings survive even if the fix is later revisited.

---

## Pitfalls learned (don't relearn)

- **Windows is case-insensitive**: `handoff.md` and `HANDOFF.md` collide. Keep one lowercase handoff.
- **SSE event naming**: the error event is named **`failed`**, not `error` — browser `EventSource`
  reserves `error` for its own connection errors. Structured events (`tool`, `tool_result`) are
  JSON-encoded so multi-line previews don't split across `data:` lines.
- **`cargo build` is not enough.** It does not compile `#[cfg(test)]` code. A trimmed "unused" import
  that only a test module uses will silently break the test suite. Always run `cargo test --workspace
  --no-run` as a separate step, then `cargo test --workspace`.
- **`cargo tree` for structural dependency claims.** If a change's point is "crate X no longer depends on
  crate Y," compile success alone does not prove the edge is gone (transitive paths still compile).
  Run `cargo tree -p X` and grep for `Y`.
- **`tokio::select!` borrow tangles**: don't reuse the same `&mut` in one branch's body that another
  branch's future borrowed; clone the channel sender, and keep rollback **inside** the awaited future
  (a Drop guard) rather than in the select arm.
- **Keep the `liberado-orchestrator` dep wherever `RuntimeFactory::runtime_for` is called** — the trait
  must be in scope. A removal that looked safe broke the build.
- The executor's **"termination follows the consumer"** design is the seam that makes
  chat-vs-autonomous a configuration, not a fork.
- **MCP server child processes have zero filesystem sandboxing** (confirmed:
  `turbomcp/crates/turbomcp-transport/src/child_process.rs` does not set `current_dir` or restrict
  environment on spawned processes). A co-resident MCP process can write directly to the vault. This is
  why writer-identity verification (hardening audit item 1) cannot be closed with a code patch — it
  needs OS-level process isolation or an out-of-band approval channel.
- **Two Rust installs on this machine**: the standalone install at `C:\Program Files\Rust stable MSVC
  1.94\` is first in PATH but lacks the wasm32 stdlib. Prepend the rustup-managed toolchain's bin dir
  before calling `dx` for WASM builds. See `agents.md` Web UI section for the exact command.

---

## Live constraints (must not violate)

- **Never print or echo `DEEPSEEK_API_KEY`** (or any secret) — confirm only length/prefix.
- `turbovault` / `turbomcp` PR branches push to the **`ForrestThump` fork only** — no upstream PRs
  without explicit permission.
- **Outward-facing actions need confirmation.**
- Don't commit, push, or run servers/daemons without being asked.
- **Never amend a prior commit** unless explicitly asked — create new commits.
- **Stage explicit file lists**, not `git add -A`/`-u` — unrelated WIP on the same branch (the
  `ui-polish` branch has uncommitted webui component work alongside hardening fixes) must not get swept
  into an unrelated commit by accident.
