---
kind: plan
status: active
authority: advisory
domain: coding-control-plane
canonical_for: daemon-pr-review-slice-4
open_items: true
---

# Daemon PR review Slice 4 — locked multi-harness plan

**Status**: active. Forrest locked the defaults in this plan on 2026-09-10. Start Slice 4a only
after the Codex-only daemon review dogfood is healthy. Open implementation work still enters
through [the backlog](backlog.md).

This plan refines [the daemon PR review kickoff](daemon-pr-review-kickoff.md). It adds one ordered
fallback route: Codex, then OpenCode, then Grok Build. Human merge remains the hard gate. Liberado
does not auto-merge.

## Locked boundary

- Worker IDs match worker kinds: `codex`, `open_code`, and `grok_build`.
- `harness_order` is an ordered list. Map iteration order never selects a worker.
- Codex stays first. OpenCode is second. Grok Build is third and disabled.
- OpenCode uses `pricing_policy = "zero_only"` until Forrest names a paid policy.
- Antigravity, Cursor local, and free-router are later candidates. They are not part of the first
  multi-harness ship.
- Shepherd owns observation, eligibility, admission, route selection, and GitHub side effects.
- The ledger owns durable command, attempt, cooldown, result, and publication facts.
- `ReviewPort` owns one harness review attempt. `ReviewPublishPort` owns GitHub review effects.
- The existing session and background execution path owns process lifecycle. Slice 4 does not add
  a workflow engine.

## Configuration sketch

```toml
[shepherd.review]
enabled = true
harness_order = ["codex", "open_code", "grok_build"]

[tuning.coder.control_plane.review_workers.codex]
kind = "codex"
executable = "/home/box/.local/bin/codex"
enabled = true

[tuning.coder.control_plane.review_workers.open_code]
kind = "open_code"
executable = "/home/box/.local/bin/opencode"
model = "PROVIDER/MODEL"
pricing_policy = "zero_only"
permission_mode = "deny_writes"
enabled = false

[tuning.coder.control_plane.review_workers.grok_build]
kind = "grok_build"
executable = "/home/box/.local/bin/grok"
model = "MODEL"
enabled = false
```

Validation rejects an unknown worker ID, a duplicate route entry, a missing route entry, and a
map key that differs from its `kind`. It also rejects enabled OpenCode without
`pricing_policy = "zero_only"` while no named paid policy exists. Disabled workers start no
process and do not receive an availability probe during a review.

## Routing and identity

One logical review command can own more than one harness attempt:

```text
review_key = repository + pull request + head SHA + policy version
command_id = one requested review for the review_key
run_id     = one worker attempt under the command
```

Write `ReviewCommandIssued` once. Give every attempted worker a new `run_id`, and use
`active_run_id` as the attempt fence. A terminal attempt clears only its matching fence. A restart
reconstructs the next route position, cooldowns, and terminal state from the ledger. It does not
repeat a finished attempt or skip an enabled worker.

`Failed` is not `Unavailable`. Keep separate ledger facts and projections:

- `Unavailable(Exhausted)` and `Unavailable(RateLimited)` may advance once to the next enabled
  worker. Persist the typed reason and `retry_after` or cooldown before admission advances.
- `Failed` stops the command. Auth errors, denied permissions, malformed or missing schema output,
  timeouts without a captured rate-limit signal, process errors, and normal model failures are
  failures.
- A future unavailable reason does not gain fallback behavior by default. Only the two locked
  typed reasons advance.
- A stale SHA, cancellation, or controller loss stops the command through its own terminal path.
  It is not worker unavailability.

Classifier order is structured protocol evidence, then exact versioned captured bytes. Exit code
alone and broad quota text do not prove exhaustion or rate limiting.

## Phase 4a — routing substrate

Add no new enabled harness in this phase.

1. Load `harness_order` as ordered policy and validate worker ID equals kind.
2. Split one `command_id` from per-attempt `run_id` values.
3. Split `Failed` from typed worker unavailability in outcomes, ledger facts, and projections.
4. Persist route position and cooldown before a fallback attempt starts.
5. Move long review execution through the existing session/background ownership. Keep the ledger
   as the durable join point.
6. Build one harness-neutral review task from `review_profile`; adapters translate that task to
   their own protocol.
7. Keep publication behind `ReviewPublishPort`. A harness cannot post, draft, push, approve, or
   merge.

Acceptance:

- Codex `Exhausted` and `RateLimited` fixtures advance at most once.
- Codex `Failed` fixtures do not start another worker.
- Restart resumes the correct command and creates a new `run_id` only when fallback is allowed.
- Reordering TOML map rows has no effect; changing `harness_order` does.
- With only Codex enabled, behavior stays equivalent to the healthy dogfood path.

## Phase 4b — OpenCode disabled and smokeable

Add `OpenCodeReviewPort` as a review adapter. Do not repair or wrap the repair
`OpenCodeWorker`. Reuse ACP transport and process containment only where their contracts fit the
review path.

The adapter must:

- receive the same pinned checkout and harness-neutral review task as Codex;
- use `permission_mode = "deny_writes"` and reject every mutating permission request;
- strip `GITHUB_TOKEN`, `GH_TOKEN`, `GH_HOST`, `LIBERADO_GITHUB_TOKEN`, and other forge
  credentials from the child environment;
- require one schema-valid review result for the expected head SHA;
- verify that the checkout is unchanged after the process exits;
- cap and retain raw output with its digest; and
- classify only captured OpenCode-specific exhaustion and rate-limit evidence as unavailable.

Keep `enabled = false`. Provide a bounded operator smoke path that can prove startup, one read-only
tool loop, schema extraction, credential stripping, write denial, timeout behavior, and unchanged
workspace state. The normal review router must not probe the disabled adapter.

Acceptance:

- A disabled OpenCode entry starts no process.
- The smoke fails on a write request, leaked forge credential, changed checkout, wrong SHA,
  malformed result, pager, or unbounded wait.
- A failed OpenCode smoke cannot alter route policy or publish a review.
- The configured provider and model satisfy `zero_only`.

## Phase 4c — opt-in OpenCode

Permit an operator to set OpenCode `enabled = true` only after the Phase 4b smoke evidence passes.
Codex remains first. OpenCode receives a fresh pinned checkout and a distinct `run_id` only after a
typed Codex `Exhausted` or `RateLimited` result.

Acceptance:

- One live, ready-and-green SHA falls back from a captured Codex availability result to one
  OpenCode attempt.
- A Codex failure, stale result, or malformed result does not start OpenCode.
- OpenCode success enters the existing publication saga with worker, command, run, policy, SHA,
  and artifact identities intact.
- Restart before and after OpenCode admission produces no duplicate attempt or publication.
- OpenCode failure stops. Its typed `Exhausted` or `RateLimited` result may advance only to the
  next enabled route entry.

## Phase 4d — Grok Build proof, still disabled

Add the Grok Build worker kind and proof path, but keep `grok_build.enabled = false`. The stale
`grok review --headless` arguments are dead. Do not copy them into code, config, tests, or
operator instructions. Discover and pin the supported unattended command from the installed CLI
and captured output.

Grok Build enablement requires all of this evidence:

1. A bounded unattended invocation starts without a pager, browser, or interactive login.
2. The invocation supports a multi-turn, tool-capable review in the pinned checkout.
3. Write denial, forge-credential stripping, and unchanged-checkout checks pass.
4. The adapter extracts one schema-valid result for the expected SHA.
5. Exact, versioned exhaustion and rate-limit signals are captured and classified.
6. Auth, permission, malformed output, timeout, process, and model failures remain `Failed`.
7. Restart and cooldown tests pass with Grok Build as the third route entry.
8. Forrest selects the model pin and explicitly approves enablement.

Phase 4d ships Grok Build proof and disabled configuration. Enabling it is a later decision after
all gates pass.

## Provider and harness boundary

OpenCode and Grok Build are harnesses: each owns a process protocol, tool loop, permission model,
and result framing. OpenRouter is a provider, not a harness. A later free-router worker can use
OpenRouter through provider HTTP policy, but it does not replace `OpenCodeReviewPort` or
`GrokBuildReviewPort`.

Free-router remains deferred past the first multi-harness ship. If it returns later, only proved
zero-price candidates are eligible unless Forrest names a paid policy. HTTP 402 belongs to that
provider route, not to Codex, OpenCode, or Grok Build classification.

## Non-goals

- Auto-merge, auto-approve, request-changes reviews, repair, push, or branch-protection changes.
- A workflow engine, trigger DSL, generic job port, or new repository catalog.
- Replacing shepherd, the ledger, `ReviewPort`, `ReviewPublishPort`, or session ownership.
- Antigravity, Cursor local, Cursor cloud, or free-router in the first multi-harness ship.
- Paid or unknown-price fallback, hidden provider substitution, or automatic policy escalation.
- Cross-harness conversation continuity or reuse of a dirty checkout.
- Grok Build enablement in Phase 4d.
- Per-review probes of disabled workers.

## Forrest open questions

These values are not locked:

1. What exact OpenCode provider/model pin and Grok Build model pin should production use?
2. Should the default durable cooldown be 900 seconds when a typed result has no reliable
   `retry_after` value?
3. What exact evidence makes Codex-only dogfood healthy enough to pass the promotion gate and
   start Slice 4a?

Do not infer answers from implementation defaults. Record Forrest's decisions before they become
production policy.
