# Hardening Audit — Proposal Integrity, Guard Coverage, Injection/Resource Surfaces (2026-07-02)

**Purpose**: Follow-on to the hygiene passes earlier this session (Tiers 1-3,
[`hygiene-audit-2026-07-02.md`](hygiene-audit-2026-07-02.md)). With the coupling/duplication backlog
exhausted, this pass asked a different question: given the pattern that produced the one real bug found
this session so far (a new feature — the runtime-level `RiskGatedToolRuntime` gate — got added without
verifying every consumer actually routes through it), where else might a similar "guard doesn't actually
cover what it's supposed to" gap exist? Three parallel research passes: (1) safety-guard coverage across
every execution path in the codebase, not just `Orchestrator`'s; (2) integrity of the propose → approve
→ execute workflow specifically; (3) classic security surfaces not yet looked at this session — command
injection, path traversal, secrets handling, resource exhaustion.

**Method**: three parallel Explore-agent passes, each given the exact architecture already known from
this session's own work (the `RiskGatedToolRuntime` gate, the proposal/approval loop, the daemon's
vault-watch pipeline) so they could check *coverage* and *integrity* rather than rediscovering basics.

**Outcome**: unlike the later hygiene tiers, most findings here held up as real and are more actionable.
Items 1-3 below are scoped for a follow-on implementation plan; items 4-9 are logged for a later pass.

---

## Items 1-3 — scoped for the next implementation plan

**Status (2026-07-02)**: items 2 and 3 are implemented (HMAC-SHA256 proposal integrity signing in
`crates/common/src/proposal.rs`'s `ProposalSigner`, verified in `handle_proposal_change` and
`execute_approved`; `RiskGatedToolRuntime`'s proposals now write to the vault's own `proposals/`
directory instead of a data-dir dead end, so runtime-gated downgrades flow through the same
approve→execute pipeline as pre-flight proposals). Item 1 remains open; see its entry below for why it
needs a separate, larger initiative (OS-level MCP process isolation or an out-of-band approval channel)
rather than a code fix in this pass.

### 1. Approval spoofing — no writer-identity verification (High) — ⏳ open, not attempted this pass

**Where**: `crates/daemon/src/lib.rs`'s `handle_proposal_change` (`~lines 220-362`, exact ranges may
have shifted). The daemon detects "a proposal was approved" purely by watching for a vault change under
`proposals/` and checking whether the resulting file content matches an *agent-attributed* audit entry
(`Vault::attribute`, `crates/vault/src/attribution.rs`) — if it doesn't match one, the change is
classified `Attribution::External` (the same classification a genuine human edit gets) and the daemon
proceeds to check `is_terminal()` / `is_expired_at()` / `is_actionable()` before calling
`execute_approved`.

**The gap**: `Attribution::External` doesn't mean "a human did this" — it means "no matching agent audit
entry was found." Any process with filesystem write access to the vault (a compromised or just-buggy
MCP server writing directly instead of through the audited `Vault::write` API, or any other local
process) can write `status: approved` into a `proposals/*.md` file and the daemon will execute it,
indistinguishable from a genuine human approval. This defeats the entire point of the propose/approve
loop — a human is supposed to be the gate on risky actions, and currently anything that can touch the
vault filesystem *is* that gate.

**Related — field tampering, not just status** (`crates/common/src/proposal.rs`'s `Proposal::from_note`,
`~lines 122-127`): the whole frontmatter struct is deserialized, not just `status`. Editing
`proposed_action` (the actual tool/args that will run), `rationale`, or `expires` between propose and
approve is silently accepted — there's no snapshot comparison against what was originally proposed.

**Why this stays open**: a content-based integrity signature (item 2, now implemented) detects tampering
with an *existing* proposal's immutable fields, but it cannot verify *who* flipped `status: approved` —
that field has to stay editable for the normal human-approval workflow, so it can't be part of the
creation-time signature. Any daemon-held signing key also lives in a plain file on the same filesystem
MCP server child processes already have full read/write access to (confirmed via
`turbomcp/crates/turbomcp-transport/src/child_process.rs` — no `current_dir`/`environment` sandboxing on
spawned processes). Closing this gap for real needs either OS-level process isolation for MCP servers
(deny filesystem access to the vault except through the audited `Vault::write` API) or an out-of-band
approval channel (push notification, CLI confirmation, authenticated web session) that a co-resident
process can't reach — both larger initiatives than a hardening patch.

### 2. Action substitution — executed action isn't provably what was approved (High) — done (2026-07-02)

**Where**: `Orchestrator::execute_approved` (`crates/orchestrator/src/lib.rs:~272-346`) reads
`proposal.proposed_action` from the in-memory struct that was *just* parsed live from the note at
approval time — not a stored, immutable snapshot of what was shown to the human when the proposal was
created. Same root cause as item 1's field-tampering note, called out separately because the fix
(cryptographic/hash commitment, or at minimum a write-once field) is a distinct piece of work from
locking down *who* can write `status: approved`.

**Implemented**: `crates/common/src/proposal.rs`'s `ProposalSigner` computes an HMAC-SHA256 over the
proposal's immutable fields (`id`/`correlation_id`/`source`/`proposed_action`/`created`) at creation
time, stored in a new `integrity` field — deliberately excluding `status`/`expires` so the normal
approval workflow still works. The signing key is per-installation (`load_or_create_proposal_key`,
`crates/config/src/lib.rs`, `<data_dir>/.proposal-key`, 0600 on Unix). Verified in
`crates/daemon/src/lib.rs`'s `handle_proposal_change` (rejects before the terminal-status check — a bad
signature leaves the proposal's on-disk status untouched rather than marking it done) and again in
`Orchestrator::execute_approved` (defense-in-depth). As documented above, this closes tampering
detection but does not solve item 1 (writer identity) — see that item's "why this stays open" note.

### 3. Runtime-level gate's own proposals are a dead end (Functional gap, not security — but directly undermines this session's own runtime-gating work) — done (2026-07-02)

**Where**: `RiskGatedToolRuntime::write_proposal` (`crates/executor/src/risk_gated.rs:~153-213`) writes
proposal files to `<LIBERADO_DATA_DIR>/proposals/proposals/<id>.md` — deliberately *outside* the vault
(so the vault watcher never reacts to them; see that file's own doc comment, and
`crates/main-agent/src/sessions.rs`'s `write_chat_proposal`, which explicitly says the same "deferred").
**Nothing reads proposal files from that directory.** The daemon only watches the vault root. There is no
UI, no polling loop, no code path anywhere that turns a runtime-level-gated proposal into an executed
action, even if a human wants to approve it.

**Why this matters more than a generic TODO**: this is the exact downgrade path
`Orchestrator::gate()` uses for `ExecuteDirect`/`DispatchSubagent`/`dispatch_parallel`'s adaptive
(non-seed) tool calls — the feature shipped earlier this session specifically to close the "adaptive
calls bypass the safety guard" gap. That fix correctly *blocks* a high-consequence adaptive call from
running immediately (`Ok("PROPOSAL CREATED...")` instead of executing) — but the promised recovery path
("a human can review and approve it later") doesn't exist. The block is real; the appeal path is not.
Both chat's own `RiskGatedToolRuntime` usage and the orchestrator's now share this same dead end.

**Implemented**: `RiskGatedToolRuntime`'s `proposals_dir` now means the vault's own `proposals/`
directory (not a data-dir dead end) — `crates/config/src/lib.rs`'s `guard_context()` builds it from the
vault path directly. The daemon's `react()` already routes any change under `proposals/` to
`handle_proposal_change` based on path alone, so a runtime-gated downgrade now flows through the exact
same approve→execute pipeline pre-flight proposals use, with no new daemon-side code. Proven end-to-end
by `crates/daemon/src/lib.rs`'s `runtime_gated_downgrade_lands_in_the_vault_and_executes_once_approved`
test: construct a `RiskGatedToolRuntime`, trigger a high-consequence call, confirm the proposal lands
under `<vault>/proposals/` with a valid signature, simulate human approval by editing status in place,
start the daemon, confirm it actually executes.

---

## Items 4-9 — logged for a later pass, not scoped yet

4. **Double-execution race** (Medium). `handle_proposal_change` has no re-entrancy guard between reading
   `status: approved` and writing `status: done` (`crates/daemon/src/lib.rs:~309` vs `~350-352`). If
   `execute_approved` is slow (a real MCP call), the debounce window (`crates/daemon/src/debounce.rs`,
   default 400ms) could expire while execution is still in flight, a second filesystem event fires for
   the same file, and a second `handle_proposal_change` runs concurrently — both would see
   `status: approved` since the done-write hasn't landed yet. Low probability under normal conditions,
   architecturally unguarded.
5. **Expiry enforced reactively only** (Low/operational). `Proposal::is_expired_at` **is** checked in
   `handle_proposal_change` (`crates/daemon/src/lib.rs:~327-330`) — a late approval on an expired
   proposal is correctly rejected. But there's no background reaper: an expired-but-untouched `Pending`
   proposal just sits there forever with no status change, since the check only runs when a new vault
   edit arrives. Not a security issue, just an operational loose end (stale proposal notes accumulate).
6. **`liberado-mcp-forge` flag-injection surface** (Low realistic severity). `crates/mcp-forge/src/build.rs`
   uses `std::process::Command` directly (no shell — command/argument injection via shell metacharacters
   is not possible), but the `package` field (`~lines 84-86`) and the `git` URL field are passed as raw
   positional/argument values to `cargo`/`git` with no validation. A `mcp-sources.toml` entry crafted
   with a leading `-` (e.g. `git = "--upload-pack=/malicious/cmd"`) would be parsed as a flag rather than
   a value. Realistic severity is low — you're the only one who edits `mcp-sources.toml` on this
   single-user system — but there's no defense-in-depth against a supply-chain edit to that file (e.g. a
   compromised dependency's install script, or your own future automation writing to it).
7. **No path-traversal validation at the Liberado layer for vault writes** (worth tracking, severity
   depends on Turbovault internals which are outside this workspace). `Vault::write`
   (`crates/vault/src/lib.rs:~91-107`) passes a tool-supplied `rel_path` straight to
   `VaultManager::write_file_with_metadata` with no `..`/absolute-path check on Liberado's side.
   `Vault::to_relative` (`~lines 148-152`) *does* correctly canonicalize+strip-prefix, but it's only used
   for watcher-delivered paths, not tool-call-argument paths headed for a write. Since tool call
   arguments ultimately come from an LLM (which could itself be manipulated via prompt injection from
   untrusted content it reads — a fetched webpage, an email), an unvalidated path argument reaching
   Turbovault unvalidated is a real theoretical vector; whether it's actually exploitable depends on
   protections inside the external Turbovault dependency, not audited here.
8. **Unbounded resource growth** (Low, slow-burn).
   - Proposal files: no archiving/rotation/deletion anywhere in `main-agent`/`daemon`/`orchestrator` —
     `<data_dir>/proposals/` and the vault's `proposals/` both grow forever across a long-running
     daemon's lifetime.
   - Conversation logs: `ConversationStore`'s `JsonlStore` (`crates/conversation-store`) is
     append-only with no `prune`/`delete` in the trait — a long-lived conversation is unbounded file
     growth.
   - Subagent recursion depth: `max_reaction_depth` (default 4, `crates/common/src/config.rs:~333`)
     gates the *daemon reaction chain* via `DispatchRequest::reaction_depth`, but nothing threads an
     equivalent counter into subagent dispatch specifically. Confirmed **not currently exploitable**
     though — a subagent's own executor turn has no access to the `Orchestrator` itself (it can only
     call MCP tools within its `subagent_budget`), so subagent-dispatching-subagent recursion is
     structurally impossible today. Worth a guard anyway if that access pattern ever changes, but not
     urgent.
   - Executor turn `Budget.max_turns` — confirmed hard cap, correctly enforced
     (`crates/executor/src/lib.rs`'s `run_loop`), no tool-callable path resets or extends it. Fine.
9. **Secrets handling** — confirmed mostly fine. `DEEPSEEK_API_KEY` never appears in a
   `CompletionRequest`, proposal file, or conversation log; it's read once into `DeepSeekProvider` and
   passed via `reqwest`'s `.bearer_auth()` (a header, not logged). One marginal, low-probability concern:
   a non-2xx DeepSeek API response's *body* is passed through verbatim into `ProviderError::InvalidRequest`
   (`provider-deepseek/src/lib.rs:~284-291`), which propagates to a tracing error log and the chat UI's
   `AgentEvent::Error` — if a hypothetical malformed/attacker-influenced API response body ever echoed a
   credential, it would surface there. No evidence this happens with DeepSeek's actual API; noted for
   completeness, not treated as an active finding.

---

## Confirmed fine — guard coverage sweep found no new gaps

The first research pass specifically hunted for *other* ungated execution paths (the same shape of bug
as the already-fixed runtime-gating gap) and found none:

- `ChatSessions` (`crates/main-agent/src/sessions.rs`) — fully gated via `build_turn_runtime` →
  `RiskGatedToolRuntime`, confirmed still correct, no regression. One low-severity design note: gating is
  convention-based (only active when `with_guards` was called) rather than type-enforced — every
  production construction path does call it (`crates/server/src/lib.rs`, `crates/bootstrap`), but a
  future caller could forget. Not urgent, just worth remembering.
- `crates/eval` — deliberately never constructs a `ToolRuntime`/`Executor` at all; it's a classifier
  accuracy probe against `Dispatcher::dispatch` only. Correctly out of scope for tool-call gating.
- Self-extension / `code-dispatch` (Phase 2) — no separate execution path exists; it's routed as a
  normal `ExecuteDirect` against the `code-dispatch` MCP, fully covered by the existing
  `Orchestrator::gate()` fix.
- `crates/daemon`'s reactive pipeline — fully covered via `Orchestrator::run`'s gated arms.
  `execute_approved` remains deliberately ungated (see items 1-2 above for why that's the part that
  actually needs work — not the gating itself, but who's allowed to trigger it).
- `crates/dispatcher/src/guards.rs`'s pre-flight coverage — no `DispatchAction` variant the classifier
  can actually emit escapes the guard check; `Propose` isn't checked, but it's a guard *output*, never a
  classifier-emitted input, so that's correct as-is.

---

## Recommended sequencing

1. **Items 1-3** — the next implementation plan (see the plan file / follow-up work). All three touch
   the same conceptual area (proposal integrity) and are worth designing together rather than as three
   separate passes.
2. **Item 4** (double-execution race) — natural next pick once 1-3 land, since fixing approval-identity
   verification will likely touch the same `handle_proposal_change` code path anyway.
3. **Items 6-9** — lower urgency, single-user-system threat model keeps realistic severity low; revisit
   opportunistically or in a dedicated later pass.
