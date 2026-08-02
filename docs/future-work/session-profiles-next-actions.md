# Session profiles — open threads for the next planning session

**Uncommitted scratch.** Parked 2026-08-01. Branch: `feat/session-profiles` @ `83a8f32`, green at
1851. **Six of nine done, one closed by measurement, one deferred on evidence.** Remaining: 4
(deferred), 5 (deferred), 6, 7.

**Not on the numbered list, and both operational:** the deployed box is seven commits behind and
still executes proposals on note-status alone — a redeploy is now a security fix, not a refresh. And
`proposals` is still `agent_writable` in `policy.toml`; the ledger makes that harmless for
authorisation, but removing the grant would be defence in depth.

## 1. ~~`liberado prompt` shows the ceiling~~ — **DONE**

The inspector now prints `scope: the profile's ceiling; a turn may hold fewer after per-goal
narrowing`, so the legitimate difference from `chat turn: tool surface` reads as a stated property
rather than a contradiction.

## 2. ~~Stale tool exchanges survive a profile switch~~ — **MEASURED, CLOSED**

The manifest is sufficient; the history rewrite is unnecessary. Full write-up in
`session-profiles-plan.md`. Measurement deleted the work, which is why it went before the build.

Surfaced and fixed one real defect: the empty manifest invited a retry it could not honour.

## 3. ~~`model` is still deferred to CH4~~ — **DONE** (`bd4f67a`)

Finished rather than fail-closed, at the operator's call. `CompletionRequest.model` is honoured at
the wire and beats a hot-swapped model; `Executor::with_model` specialises one per turn so a session
choosing a model cannot change it for anyone else. Five tracing spans that reported the provider's
model were reporting one the request did not use — also fixed.

## 4. Mutation testing has no CI ratchet

`just mutants <crate>` is a manual recipe; `grep -c mutants .github/workflows/ci.yml` → **0**. Catch
rates (7%, 27%, 93%, 97%) live in markdown and will rot silently. A number nothing enforces is a
snapshot, not a guard.

`cargo mutants` is too slow per-PR; a weekly scheduled job over two or three crates would make the
figure defend itself. Right now the docs assert a quality level that nothing checks.

## 5. `liberado prompt --live`

The config-only version cannot expand a whole-server grant — it prints `<mcp>:*`. A `--live` variant
against a running daemon would resolve real tool names by reading the same catalog the manifest does,
and can share `compose_chat_prompt`'s renderer.

Deliberately not built first: the config version is the one that runs in CI and mid-debug, before
deploying, which is when it is actually needed. Worth adding once there is a second reason to want it.

## 6. Session polish

Unscoped. Candidates seen while working: the WebUI still ships an **unoptimized wasm bundle**
(`wasm-opt` crashes with `0xc0000409` during `deploy-webui-homelab.ps1` and the build falls through);
the profile picker and chip are functional but unreviewed for mobile; `/api/profiles` returns
`model` that nothing applies (see 3).

## 7. What a PR to main should contain

The branch has grown well past "session profiles": steps 1–7, the merged mutants/proptest/docs
branch (74 commits), the clock hardening, the budget-resource fix, the seam sweep's descendants.
Worth deciding whether that lands as one PR or is split — and specifically whether the docs
reorganisation should go separately, since it touches 141 files and would otherwise bury the
behavioural changes in review.

No rush stated; the branch is deployed and green (1834 passed, 24 ignored).

## 8. ~~Approval-bot retry loop~~ — **DONE**

Not a loop: three taps on a proposal the daemon had already archived. The duplicate *prompts* were
the real bug (fixed in `1ba9eb3` — one intent now reuses its pending proposal). The remaining half
was the reporting: a tap on a resolved proposal logged a vault I/O error and told the operator
"Proposal not found" about something approved seconds earlier. The bot now checks
`proposals/archive/<outcome>/` and answers "Already approved."

## 9. ~~Approval state lives somewhere the agent can write~~ — **DONE** (option A)

Approval decisions moved to an append-only ledger under `<LIBERADO_DATA_DIR>/approvals.jsonl`, which
no MCP mounts and no tool addresses. The proposal note is now a **view**: the daemon reads
`status: approved` and then requires a matching ledger entry before executing. Editing the note — in
Obsidian, over Syncthing, or by an agent — authorises nothing.

Fail-closed throughout: a missing, unreadable, or corrupt ledger authorises nothing, and a daemon
built without one executes nothing. The Telegram bot records the decision *before* touching the note,
so a ledger write failure means the decision did not happen rather than leaving a note that claims
otherwise.

Four existing tests had to record a decision to keep passing, which is the contract change working
as intended — they had been relying on the note alone.

