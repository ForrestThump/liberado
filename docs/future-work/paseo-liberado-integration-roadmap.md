# Paseo ↔ Liberado integration — ordered gap plan

**Status:** Living plan (2026-08-09). Core ACP path **shipped** on
`feat/paseo-liberado-integration`; this doc is the ordered backlog for making Liberado
reliably usable through the [ForrestThump/paseo](https://github.com/ForrestThump/paseo) fork
(and stock Generic ACP), then for optional productization and remote access.

**Not a single PR.** One PR-sized slice at a time; mutation evidence for behaviour claims per
[`backlog.md`](backlog.md).

**Related:**

- Dogfood / install: [`../impl/paseo-integration.md`](../impl/paseo-integration.md)
- Prior residual list (folded into this plan): [`acp-bridge-completion-roadmap.md`](acp-bridge-completion-roadmap.md)
- Install script: `scripts/install-paseo-liberado.ps1`
- Config example: `config.example/paseo-liberado.json`
- Bridge source: `crates/acp-bridge/`
- Scoreboard: [`../roadmap.md`](../roadmap.md) (cross-cutting Paseo items)

---

## Deliverable (definition of done)

End state the integration is aiming at:

1. **Deploy the Liberado daemon** (`liberado serve`) on a host.
2. **Install and deploy the Paseo fork** (ForrestThump/paseo) on the same or a client host.
3. **Paseo’s server detects Liberado** as an available coding agent (provider diagnostics green:
   launcher binary, ACP `initialize`, ACP `session/new` — and later, any daemon presence probe
   we add for Track B).
4. **Point a Liberado coding instance at a specific folder** — Paseo starts a Liberado session
   whose workspace/`cwd` is that folder (coding tools rooted there; agent works in that tree).

Track A (ACP `liberado-acp` spawned by Generic ACP) is the **supported local path** to (3)–(4)
today: detect via provider config + diagnostics; folder = session `cwd` from Paseo’s workspace
picker. Track B (daemon tunnel / remote) must still land for “daemon already running, Paseo
attaches remotely” without spawning a one-shot ACP process — see Phase 6. Both tracks share the
same product bar above.

---

## Goal and non-goals

**Goal:** Meet the deliverable above: deploy daemon + Paseo fork, detect Liberado, run coding
against a chosen folder — with correct tool streaming, honest resume, green diagnostics, and a
clear path to remote attach and fuller Liberado parity.

**Non-goals for early slices:**

- Replacing Paseo’s tunnel stack or building a custom Paseo UI.
- Shipping Liberado’s full goal/gate/worktree coding pack through ACP in the first pass
  (main-agent `Conversation` + `CodingToolRuntime` is the v1 surface).
- Embedding API keys in Paseo `config.json`.

---

## Architecture (two tracks)

| Track | What it is | State |
|-------|------------|--------|
| **A — ACP coding agent** | Paseo spawns `liberado-acp` over stdio (`extends: "acp"`). Same pattern as Gemini/Hermes. Session `cwd` = coding folder. | Core path landed; gaps below |
| **B — Remote daemon** | Paseo detects/attaches to a deployed `liberado serve` (tunnel / host access, HTTP/SSE). | Not started; required for full “daemon already up” deliverable |

Stale `paseo/packages/server/dist/.../liberado-*.js` artifacts are from an earlier experiment
(daemon spawn + first-class ACP client). **Not source of truth** — ignore or delete on next
Paseo rebuild. Do not design against dist-only files.

**Paseo fork today:** effectively upstream main + local tweaks (e.g. worktree port cast). No
first-class Liberado provider in source; registration is user `~/.paseo/config.json`.

---

## What works today (do not rebuild)

- JSON-RPC 2.0 NDJSON on stdin/stdout; logs on stderr
- `initialize` → `protocolVersion: 1`, `agentInfo`, `agentCapabilities`
- `session/new` → session id + models/modes; coding tools rooted at `cwd`
- `session/prompt` → `Conversation::turn_stream` + `session/update`
  (`agent_message_chunk`, `tool_call`, `tool_call_update`)
- `session/cancel` → abort in-flight turn → `stopReason: "cancelled"`
- Provider from `DEEPSEEK_API_KEY` / OpenRouter / OpenAI or `LIBERADO_CONFIG_DIR`
- Install + config merge script; example provider block with `supportsMcpServers: false`
- Unit tests for prompt extraction + session payload shape

---

## Ordered plan

Priority is dogfood impact, then honesty of capabilities, then product polish. **Do not
reorder P0/P1 without a written reason** — Paseo’s Generic ACP client already assumes the
shapes we claim.

### Phase 0 — Dogfood-blocking wire correctness (Liberado `acp-bridge`) — ✅ **all landed**

**Verified complete 2026-08-10.** All three shipped; the table below is kept for the reasoning, not
as open work.

- **P0.1** — `push_tool_call_id` / `pop_tool_call_id` in `crates/acp-bridge/src/wire.rs`, LIFO so
  same-named concurrent calls pair correctly. Three unit tests plus a MockProvider stream assertion.
- **P0.2** — chose honesty: `LOAD_SESSION_CAPABILITY = false`, and `session/load` returns an error
  rather than silently opening a fresh conversation. Durable load is Phase 3.
- **P0.3** — `handle_cli_args` runs **before** stdin is touched, so `--version` / `--help` cannot
  hang a probe.


Ship these before treating the integration as “ready to daily-drive.”

| # | Slice | Why | Acceptance |
|---|--------|-----|------------|
| **P0.1** | **Tool-call id correlation** | Start emits `call-…`, finish emits a different `done-…`. Paseo indexes tool UI by `toolCallId`, so finishes never attach → stuck pending tools. | Same `toolCallId` on `tool_call` and matching `tool_call_update`. Mutation: break pairing → test fails. |
| **P0.2** | **Honest or real session resume** | Bridge advertises `loadSession: true` but `session/load` opens a **fresh** conversation. Paseo prefers `loadSession` when the flag is set → “resume” with wiped memory. | **Either** (a) `loadSession: false` until durable history exists, **or** (b) durable load that restores history (prefer (a) first if (b) is multi-PR). Document which. |
| **P0.3** | **`--version` / CLI hygiene** | Generic ACP diagnostics run `liberado-acp --version`. Binary ignores argv and waits on stdin → failed/timeout version row. | `liberado-acp --version` prints version and exits 0 without reading stdin. Same for `--help` if cheap. |

**Suggested PR split:** P0.1 alone (small, high value); P0.2 + P0.3 can share a PR if both stay tiny (flag flip + clap/argv).

---

### Phase 1 — Regression net (Liberado)

| # | Slice | Why | Acceptance |
|---|--------|-----|------------|
| **P1.1** | **Mock-provider stream integration test** | Current unit tests never assert streamed `session/update` pairing. P0.1 would have been invisible. | Drive `MockProvider` (or lib-exported request loop) through prompt + tools; assert text chunks and tool start/finish share ids. Prefer library-exported handler over only-stdio if that keeps the test hermetic. |
| **P1.2** | **Stdio smoke for initialize + session/new** | Install script already pipes initialize; codify in CI if not already under `cargo test -p liberado-acp-bridge`. | Automated, no network, no API key required for initialize/new. |

---

### Phase 2 — Modes and model selection that do something (Liberado) — ✅ **landed, differently than specced**

**Superseded 2026-08-09.** Both slices shipped, but P2.1 was answered with a *different* design than
the one written below, so read the code before trusting the row.

- **P2.1** — `session/set_mode` switches between **coding / chat / face** (`AgentMode`), three
  engines on one Paseo provider, not the plan/explore PathPolicy presets specced here. The presets
  still exist in `coder-core` and remain available if a read-only ACP mode is ever wanted; nothing
  currently maps to them.
- **P2.2** — `session/set_model` hot-swaps the active model against the live OpenRouter catalog.

| # | Slice | Original acceptance (kept for the record) |
|---|--------|------------|
| **P2.1** | Map `session/set_mode` → PathPolicy presets | At least `code` (default full tools) + `explore` or `plan` (read-only / plan-artifact). |
| **P2.2** | `session/set_model` wired | Changing model affects subsequent `complete` calls for that session. |

---

### Phase 3 — Durable ACP sessions (Liberado)

P0.2 chose honesty (`loadSession: false`), so this phase is unblocked and **is the next real work in
this roadmap**.

**Attempted twice by the coding pack (2026-08-10), not landed.** Split P3.1 into a storage-only
slice first — call it **P3.1a** — because it is safe to build and safe to review:

> Add `crates/acp-bridge/src/session_store.rs` with a serialisable record (id, mode, cwd, model,
> messages, `updated_at`) under `<LIBERADO_DATA_DIR>/acp-sessions/`. Atomic writes (temp + rename).
> Treat the client-supplied session id as untrusted when forming a filename. Wire `session/new`,
> a completed `session/prompt`, `session/set_mode` and `session/set_model`. A persistence failure
> must log and continue, never fail the turn.
>
> **Do not** flip `LOAD_SESSION_CAPABILITY`, make `session/load` succeed, or add replay. Those are
> P3.2/P3.3 and a wire-behaviour change here would break a live editor integration.

The first attempt died on a full disk plus the `git_diff` blindness fixed in
[#118](https://github.com/ForrestThump/liberado/pull/118); its partial output is preserved on branch
`lib-18ca8a53fbbd54f4-20612`. Prefer re-running to salvaging. Background:
[`coder-harness-reliability-2026-08.md`](coder-harness-reliability-2026-08.md).

| # | Slice | Why | Acceptance |
|---|--------|-----|------------|
| **P3.1** | **Persist conversation under session id** | Process-local only today. | Restart of `liberado-acp` + `session/load` restores messages for that id (store under data dir, not vault MCP). |
| **P3.2** | **History replay on load** | Paseo resume expects history via session updates / loaded state, not an empty transcript. | After load, client sees prior user/assistant content (shape aligned with ACP load semantics Paseo uses). |
| **P3.3** | **Re-enable `loadSession: true`** | Only when P3.1–P3.2 pass. | Capability flag matches behaviour; dogfood resume in Paseo. |

Prefer composing existing session-store machinery over inventing a parallel store. If that
pulls daemon layering into the bridge uncomfortably, a small file-backed ACP session store in
the bridge crate is acceptable with an explicit follow-up to converge.

---

### Phase 4 — Optional wire polish (Liberado)

| # | Slice | Why | Acceptance |
|---|--------|-----|------------|
| **P4.1** | **Error stop reasons** — ⚠️ **open, but the acceptance below is unachievable as written** | Provider/turn failures still return `stopReason: "end_turn"` with an “Error:” text prefix (three sites in `main.rs`). | ACP's `StopReason` is a **closed set** — `end_turn`, `max_tokens`, `max_turn_requests`, `refusal`, `cancelled` — with **no error variant** (confirmed against `zed-industries/agent-client-protocol`, 2026-08-10). So "use a distinct stop reason where ACP allows" has no answer. The real choice is between returning a JSON-RPC error for harness failures and documenting the `end_turn` mapping deliberately. **Decide that first**; do not dispatch this as an agent task until it is decided, because the done-condition is a judgement call. |
| **P4.2** | ~~**Stdout serialization**~~ | — | ✅ **Landed.** `StdoutWire` in `crates/acp-bridge/src/wire.rs` is a single write path under one lock; responses and notifications both go through it. |
| **P4.3** | **Richer prompt blocks** | Text + weak resource URI only; image/audio correctly advertised false. | Embedded context blocks produce useful tool-facing text; no fake image support. |
| **P4.4** | **Permission mapping** | Optional: Paseo `requestPermission` for Liberado write/execute gates. | Only if dogfood shows Paseo-mediated approvals are needed; default remains Liberado-internal policy. |

---

### Phase 5 — Paseo fork productization (ForrestThump/paseo)

Does not block Liberado P0–P1. Do on the fork after Liberado dogfood is green.

| # | Slice | Why | Acceptance |
|---|--------|-----|------------|
| **P5.1** | **First-class provider definition** | Config-only `extends: "acp"` works but is invisible out of the box. | Built-in or documented one-click: label Liberado, default `command: ["liberado-acp"]`, `supportsMcpServers: false`, correct capability flags (no fake persistence until P3). |
| **P5.2** | **Icon + catalog entry** | Other ACP agents appear in hub/catalog with icons. | Liberado appears in provider list/catalog with icon asset. |
| **P5.3** | **Clean stale dist** | `liberado-agent.js` / `liberado-acp-agent.js` in dist confuse agents and humans. | Rebuild without orphan sources, or remove dead artifacts. |
| **P5.4** | **Keep fork synced** | Fork tracks getpaseo; local tweaks (e.g. worktree typing) live on `feat/*` branches. | Document: `origin` = ForrestThump, `upstream` = getpaseo; rebase cadence. |

---

### Phase 6 — Track B: remote access via Paseo (separate program)

Roadmap item “Remote access via Paseo” — **not** the ACP binary.

| # | Slice | Why | Acceptance |
|---|--------|-----|------------|
| **P6.1** | **Vet Paseo tunnel + host model** | Need a stable way to reach a machine running Liberado. | Short design note: what Paseo exposes (tunnel, daemon host, ports) vs what Liberado needs (`liberado serve` bind, auth). |
| **P6.2** | **Remote profile / docs** | Operators need a single recipe. | Doc: run daemon, expose via Paseo, open chat/coding against remote workspace. Prefer config/env over new flags unless a flag is clearly better. |
| **P6.3** | **Dogfood remote coding session** | Proves the path. | One recorded remote session (can be ACP against remote cwd or HTTP client — pick one and stick to it). |

Do **not** revive dist-only “spawn liberado serve from Paseo provider” without a design review;
ACP Track A is the supported coding entry point for local use.

---

### Phase 7 — Deeper Liberado parity (optional, large)

Only after P0–P3 feel good.

| # | Slice | Notes |
|---|--------|--------|
| **P7.1** | Coding-pack path through ACP | Swap or dual-path `Conversation` → coder-agent session (goals, gates, worktrees). Large; may need ACP modes for goal lifecycle. |
| **P7.2** | Checkpoints / preflight surface | Expose only if Paseo UX can show them; otherwise keep inside Liberado. |
| **P7.3** | Vault MCP / face-agent | Explicitly out of ACP bridge v1; do not smuggle vault grants into coding cwd tools. |

---

## Dependency graph (build order)

```
P0.1 tool ids ──┐
P0.2 resume honesty ──┼──► P1.1 mock stream test (locks P0.1)
P0.3 --version ───────┘         │
                                ▼
                         dogfood green
                                │
              ┌─────────────────┼─────────────────┐
              ▼                 ▼                 ▼
           P2 modes          P3 durable        P5 fork polish
              │                 │                 │
              └────────┬────────┘                 │
                       ▼                          │
                  P4 polish                       │
                       │                          │
                       └──────────┬───────────────┘
                                  ▼
                           P6 remote (Track B)
                                  │
                                  ▼
                           P7 coder-pack parity (optional)
```

---

## Verification rules (every Liberado slice)

1. **Mutate the claim.** If the test still passes after removing the fix, the test is wrong.
2. **`--no-fail-fast`** when collecting full failure sets.
3. **No API keys in fixtures or Paseo config examples.**
4. **Stdout is the wire** — never log to stdout from the bridge.
5. **Capabilities must match behaviour** — especially `loadSession` and MCP flags.
6. **Windows is first-class** for install script and PATH/`liberado-acp.exe` dogfood.

---

## Operator checklist (until P0 is done)

```powershell
# Liberado repo
cargo install --path crates/acp-bridge --force
powershell -File scripts/install-paseo-liberado.ps1
# Restart Paseo; provider Liberado; diagnostics: binary, initialize, session/new
# Prompt only after DEEPSEEK_API_KEY (or peer) is in the env that starts Paseo
```

Known dogfood friction until fixed:

- Tool rows may not complete cleanly in UI (P0.1)
- Resume may wipe chat (P0.2)
- Version diagnostic row may fail (P0.3)

---

## Done when

**Minimum (integration “usable”):** P0.1–P0.3 + P1.1 green; install script dogfood on Windows
with Paseo fork; tool timeline pairs; no false resume promise.

**Comfortable:** + P2 modes, P3 durable load, P5 first-class provider on fork.

**Remote story:** P6 complete — separate from ACP comfort.

Update this file’s status header and the Paseo bullets in [`../roadmap.md`](../roadmap.md)
when phases land.
