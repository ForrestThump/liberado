---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0011
open_items: false
---

# ADR-0011: Human-in-the-Loop / Proposal & Approval Boundary

| Field | Value |
|-------|-------|
| Status | accepted |
| Date | 2026-07-02 (from consolidated decision log; see git history) |
| ID | ADR-0011 |

## Context

See **Full historical body** for the original framing, open questions, and design discussion.

## Decision

A Proposal is a structured vault artifact (typed output of dispatch guards and write classes). High-consequence or proposal_only actions write proposals rather than acting. Approval is a human vault edit (or TUI) that the daemon executes with the proposal's correlation_id; terminal proposals archive out of the active proposals directory. Default posture is conservative (propose, don't act).

## Consequences

Full propose/approve/execute loop is closed for concrete high-consequence tool paths. Fuzzier cases may still Clarify. Active proposals/ stays reviewable.

## Rejected alternatives

Fully autonomous high-consequence external actions without a proposal. Classifier-judged approval instead of zone/consequence/guard rules.

## Implementation and tests

- `liberado-dispatch-logic-spec.md`
- `liberado-vault-concurrency-spec.md`
- `proposals/<id>.md`

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated decision log)
- **Superseded by:** (none)

## Full historical body

The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.

---

**Why it matters**: Some background actions (especially involving family, schedule, or external communication) should not be fully autonomous.

**Recommended path**:
- Define a clear **"proposal" output type** early.
- High-consequence actions emit proposals into a review location in the vault (or a dedicated inbox) rather than acting directly.
- Start conservative: most hook reactions write proposals or structured notes; only low-risk actions execute directly.

**Status**: Complete

Decision 11: A **Proposal** is a structured vault artifact — the typed output already referenced by the dispatch guards (`liberado-dispatch-logic-spec.md` §6) and concurrency write-classes (`liberado-vault-concurrency-spec.md` §3).

- **Shape**: a note in `proposals/` with frontmatter `{ id, correlation_id, source (agent/hook name), proposed_action (structured), rationale, status: pending|approved|rejected|expired, created, expires }` and a human-readable body.
- **What requires one** (computed, not classifier-judged): any write to a `proposal_only` zone, any high-consequence action (external comms, irreversible deletes, anything touching `Sensitive`/`FamilyShared`), and any guard-forced downgrade. Unlisted zones default to `proposal_only` (fail safe), so the **conservative default is "propose, don't act."**
- **Approval lifecycle (closes through the same machinery)**: agent writes proposal ? ContextPolicy Job B surfaces it ? user approves via the **TUI command *or* by editing `status: approved`** (so approval also works directly from Obsidian) ? the approval is a human-sourced vault write that the daemon's subscription picks up ? the daemon executes the `proposed_action` (now authorized) with the **proposal's `correlation_id`**, marks the proposal `done`, and links the resulting artifact. The execution write is agent-sourced and de-looped normally (concurrency spec §6). Expired/rejected proposals are never executed.
- **Archived on resolution**: the moment a proposal goes terminal (approved?`done`, or `rejected`/`expired`), the daemon moves the note out of the active `proposals/` dir into `proposals/archive/<outcome>/` (`approved`/`rejected`/`expired`), so the active dir shows only what still needs a human. The move carries agent (`DAEMON_SOURCE`) provenance so attribution suppresses it and the archived note never re-enters the pipeline (`react()` also excludes the whole `proposals/archive/` subtree). Best-effort — the terminal status is already persisted in the note's frontmatter, so a note that fails to archive is left in place, never lost or re-executed.
- **v1 conservative posture**: most hook reactions emit proposals or plain structured notes; only explicitly low-risk, `shared`/`agent_writable` actions execute directly.

**Status update (emit AND approve?execute landed, June 24, 2026)**: the full propose?approve?execute loop is closed. The EMIT path is wired — high-consequence *concrete* actions (an `ExecuteDirect` with a non-empty seed call list whose MCP is `External`/`Irreversible`) downgrade through `DispatchAction::Propose` ? `Disposition::Propose(Proposal)` ? a `proposals/<id>.md` artifact. The daemon writes it with **agent provenance**, so attribution suppresses the write (no self-reaction). On the APPROVE?EXECUTE side, a human's `status: approved` edit is picked up by the daemon's watch loop: `react()` checks for the `proposals/` path prefix before dispatching, routes to `handle_proposal_change`, which parses the frontmatter, validates it is Approved + non-expired + non-terminal, then calls `orchestrator.execute_approved()` with the proposal's `correlation_id` as provenance. Execution runs the approved `ToolCalls` directly against a runtime scoped to their MCPs (no classifier, no guards — the human edit is the authorization). On success the daemon flips `status` to `done` with agent provenance (loop-broken), then archives the note to `proposals/archive/approved/` so the active `proposals/` dir doesn't accumulate resolved notes; a human deny (`status: rejected`) is archived to `proposals/archive/rejected/` from the terminal-observe branch the same way (`archive_terminal_proposal`). Idempotency: terminal proposals (Done/Rejected/Expired) and non-actionable proposals (Pending) are left alone; infra errors from execution propagate (not marked done, retriable on the next watch cycle). Fuzzier high-consequence cases (empty-seed `ExecuteDirect`, `DispatchSubagent`, the magnitude-gate goal signal) still downgrade to `Clarify` for now.
