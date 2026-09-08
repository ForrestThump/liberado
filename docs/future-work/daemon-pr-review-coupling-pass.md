---
kind: finding
status: active
authority: advisory
domain: coding-control-plane
canonical_for: daemon-pr-review-coupling-pass
open_items: true
---

# Sol coupling pass — daemon PR review after Slice 2

**Status**: advisory sibling to
[`daemon-pr-review-kickoff.md`](daemon-pr-review-kickoff.md). It reviews main at `4be02711`, after
PRs #244 and #247. It does not authorize application-code changes.

## Verdict

Ship Slice 3 as the publication saga. Do not put a generic trigger/task abstraction in front of
it.

Slices 0–2 already contain the two useful narrow seams:

- `review_eligible` plus `ObserverIntent::Eligible` separates eligibility from execution.
- `ReviewPort::invoke(ReviewInvokeRequest)` separates one review task from its Codex adapter.

Those seams are PR-review specific, which is correct. They are not tied to GitHub publication.
Add a separate GitHub publication port and a small saga coordinator in Slice 3. Do not turn
`ReviewPort` into `JobPort`, and do not add a trigger DSL or workflow runtime.

The design is not blocked by overfitting to ready-and-green. The code does have several false
config seams and Slice 4 blockers. Close the write-safety gaps in Slice 3. Close the routing and
attempt-model gaps before the second worker is enabled.

## What landed

The implementation has a sound basic split:

| Concern | Current seam | Assessment |
|---|---|---|
| GitHub observation | `pr_review_poll` and `pr_review_observer` | Keep. Events wake a state re-read. |
| Eligibility | `review_eligible`, `reconcile_snapshot` | Keep. It is pure and shared with CLI dry-run. |
| Durable state | coding task ledger and review-specific events | Keep, but strengthen publication and attempt identities. |
| One review invocation | `ReviewPort` and `ReviewInvokeRequest` | Keep. This is the thin harness-neutral task port. |
| Codex process | `CodexReviewPort` | Keep as one adapter, not as the routing layer. |
| Admission | `pr_review_admission` and `active_run_id` | Keep the fence, but split command identity from attempt identity before fallback. |
| Publication | none | Add in Slice 3 as a separate effect port and saga. |

Good safety properties are already mechanical. The checkout is pinned and checked for changes.
Forge environment variables are removed. The result has a versioned schema and reviewed SHA.
The observer does no GitHub write. Disabled Codex starts no process. Repair events and approval
semantics are not reused.

## Coupling findings

### 1. The documented routing policy is not config yet

The canonical example puts `harness_order` in `[shepherd.review]`, but
`ShepherdReviewConfig` has no such field. It also lacks `review_event`, `blocker_action`, and
`on_synchronize`. Unknown fields are accepted in that struct, so these values can be present in
TOML and have no effect.

`review_workers` is a `BTreeMap`. `maybe_dispatch` scans that map for an enabled Codex worker. It
does not use user order and it cannot select another kind. This behavior was acceptable for the
one-Codex Slice 2, but it is not a routing contract.

The Codex example also declares `output` and `sandbox`, but the loaded Codex variant has only
`executable` and `enabled`. Because that enum denies unknown fields, the canonical example and the
implemented config vocabulary do not agree. Either add and consume the fields or remove them from
the example. Read-only sandbox must remain a non-overridable minimum even if a field is added.

Small correction:

- Keep adapter definitions in `tuning.coder.control_plane.review_workers`.
- Add `harness_order: Vec<String>` to `shepherd.review`, because shepherd owns admission and
  fallback order.
- Reject duplicate or unknown IDs and enabled workers that are absent from the order.
- Add config-arrival tests. Add `deny_unknown_fields` after the planned fields exist.
- Keep one global order for now. Add a per-project override only when a real repository needs it.

Do not use map iteration as policy order.

### 2. The gate is named in config, but not selected by config

The project row requires the literal string `gate = "ready_and_green_tip"`. The observer always
runs that policy. This is a safe closed world for the first gate, but it is not yet a configurable
predicate seam.

Do not fix this with a general expression language. When a second gate is ready, parse a small
typed enum and dispatch to pure evaluators:

```text
ReviewGateKind::ReadyAndGreenTip -> ready_and_green_tip(snapshot, facts, checks)
```

A poll, webhook, CLI command, or later cron tick must only cause reconciliation. The selected gate
must decide eligibility from current state. This preserves the present event-as-wake rule and lets
later trigger sources use the same path.

For clarity, a later config migration can rename project `gate` to `review_gate`. This rename is
not worth delaying Slice 3. An alias can keep old config valid.

### 3. The review task seam is adequate, but its request needs one later extension

`ReviewInvokeRequest` already binds repository, PR, base SHA, head SHA, workspace, schema, task,
command, and run. That is enough for Codex publication.

OpenCode and other harnesses need common review instructions. Before the OpenCode adapter lands,
add a harness-neutral prompt or `ReviewTaskSpec` to this request. Build it once from the selected
review profile and bounded diff metadata. Each adapter can then translate the same task into its
own protocol.

The current `review_profile` config is required but is not consumed by the daemon review path.
Make it select those common instructions before claiming that profile customization works. Do not
create a generic “run this job” API until a second non-review task proves the common fields.

### 4. Command and run identity are too tightly coupled for fallback

Slice 2 requires `command_id == run_id`. Admission also rejects every later issue of a command
once `ReviewCommandIssued` exists. This gives at-most-once Codex execution, but it cannot represent
Codex exhaustion followed by an OpenCode attempt for the same review key.

Before Slice 4, use these identities:

```text
review_key = repository + PR + head SHA + policy version
command_id = one requested review for that review_key
run_id     = one worker attempt under that command
```

Record `ReviewCommandIssued` once and `ReviewRunStarted` once per worker attempt. Keep
`active_run_id` as the open attempt fence. Terminal attempt events clear only their matching run.
The command stays open after typed unavailability so routing can select the next enabled worker.

This is a review-attempt model, not a workflow engine.

### 5. Failure and unavailability are currently one ledger fact

`ReviewInvokeOutcome::Failed` is recorded as `ReviewWorkerUnavailable`. That loses the rule that
typed quota or service unavailability may fall through, while malformed output, auth failure,
permission failure, and normal model failure stop the route.

Before fallback, add `ReviewRunFailed` and reserve `ReviewWorkerUnavailable` for classified,
retryable availability failures. Add `retry_after` or a durable cooldown reference to the latter.
Add `Cancelled` before execution moves to a background lifecycle.

### 6. The polling task directly runs Codex

`maybe_dispatch` creates a worktree and synchronously invokes Codex inside the polling pass. A long
review therefore delays later repositories in that pass. A repository API error also returns from
the project loop before later repositories run.

Do not redesign this before COMMENT publication. The first production configuration has one
global review at a time. However, do not deepen this coupling in Slice 3:

- Publication must consume terminal ledger evidence, not the return value of the Codex process.
- Isolate one project's poll error so other configured repositories still reconcile.
- After Slice 3, route invocation through the existing session/background execution ownership, or
  a thin review-run service that uses it. Keep the ledger as the durable join point.
- Define recovery for an issued run with no terminal event. At-most-once alone can leave the PR
  blocked forever after a daemon crash.

This is a lifecycle correction. It does not require a new orchestrator.

### 7. Publication idempotency must include policy version

The locked rule is one normal review per repository, PR, SHA, and policy version. The current
cycle projection remembers only `accepted_sha`. The planned GitHub marker also contains only
repository, PR, and SHA. A policy-version change would either stay blocked by the cycle or find the
old GitHub review and treat it as the new one.

In Slice 3, project an accepted `review_key`, not only an accepted SHA. Put the policy version or a
stable review-key digest in the immutable GitHub marker. Record `policy_version`, `command_id`,
`run_id`, `worker_id`, and `artifact_digest` with `ReviewPublished`. This also makes audit and
recovery independent of string parsing in the command ID.

### 8. Write identity is not closed yet

`expected_login` exists but is optional and is not required by native-review validation. That is
safe while the observer is read-only. It is not safe for Slice 3.

For an active writer, Slice 3 must require a nonempty `expected_login`, resolve `GET /user` at
startup, and resolve it again immediately before each GitHub write. A mismatch stops that write.
The declared permission contract must also add pull-request write permission for the write slice.

### 9. Prove real Codex output extraction before it can publish

The Slice 2 process tests use a fake executable that writes one raw `ReviewResult` JSON object.
The production command also requests Codex JSON output, while `classify_output` parses all stdout
as that one object. A real Codex JSON event stream would not match that test fixture.

Before Slice 3 enables GitHub writes, capture one live successful Codex invocation and prove how
the final schema result is extracted. If stdout is JSONL, parse the documented final-result event
and reject missing, duplicate, trailing, or oversized result payloads. Keep the frozen raw-object
fixture only if the installed CLI truly emits that shape for this command.

## Slice 3: smallest safe path

Keep Slice 3 as “publication saga only.” Its input is a terminal `ReviewRunFinished` artifact for
an eligible review key. Its output is ledger facts about external effects.

### A. Close config and identity before enabling writes

1. Load and validate the locked `COMMENT`, checklist-C, draft-on-blocker, synchronize-to-draft,
   and `expected_login` settings. It is also valid to keep the first three as typed constants if
   the TOML keys are removed from the example. Do not accept config that appears to override them
   but is ignored.
2. Add `harness_order` as real config, even though Slice 3 still admits Codex only. This prevents a
   false operator control and prepares Slice 4 without changing execution.
3. Prove the real Codex final-result framing before its artifact can enter the publication saga.
4. Require the active controller lease and exact current head before every effect.

### B. Add a publication effect port

Add a review-specific port, such as `PullRequestReviewEffects`, implemented by the GitHub adapter.
It should expose the small operations the saga needs:

```text
read PR and current head
read authenticated login
find COMMENT review by immutable review-key marker
create COMMENT review at commit_id
find checklist comment by immutable review-key marker
create checklist comment
find synchronize note by old/new SHA marker
create synchronize note
convert the still-matching PR to draft
```

The saga coordinator decides which operation is next. The port performs GitHub I/O. Do not give
publication methods to `ReviewPort`, and do not let a harness post to GitHub.

### C. Resume one missing effect at a time

For a finished review:

1. Load the structured artifact by its recorded digest and validate its schema and SHA again.
2. Re-read PR state, authenticated login, and head SHA.
3. Find or create the immutable `COMMENT` review. Put only valid in-diff lines inline. Put all
   other findings in the body.
4. Append `ReviewPublished` with the full review identity.
5. If blockers exist, find or create the separate checklist issue comment and append its fact.
6. Re-read identity, PR state, and head. Convert to draft only for the same SHA, then append the
   draft fact.
7. Process synchronize intent with its own old/new SHA note marker and same-head draft guard.

Every “find or create” must search GitHub before creation. This closes the crash window where the
GitHub request succeeded but the ledger append did not. A retry performs only the missing effect.

The artifact location must be durable and derivable from ledger state. Do not make publication
depend on an in-memory return value or an undocumented worktree-relative path.

### D. Slice 3 acceptance additions

Keep the canonical acceptance tests and add these:

- A config value for publication or worker order is either consumed or rejected.
- A login mismatch before each kind of write produces no write.
- A crash after GitHub success but before ledger append finds the marker and records the existing
  object without creating another.
- A policy-version change on the same SHA has the locked one-per-policy behavior.
- One repository's API failure does not stop reconciliation of the next configured repository.
- Publication after daemon restart reads the terminal artifact from durable state.
- A captured real Codex success produces exactly one schema-valid artifact; JSON event framing is
  not approximated by a fake raw-object stream.

## OpenCode placement

OpenCode should be an explicit review worker kind in Slice 4:

```toml
[shepherd.review]
harness_order = ["codex", "opencode", "free-router"]

[tuning.coder.control_plane.review_workers.opencode]
kind = "open_code"
executable = "/home/box/.local/bin/opencode"
model = "PROVIDER/MODEL"
permission_mode = "deny_writes"
enabled = false
```

The exact `kind` spelling should follow the repository's serde convention. The important point is
that OpenCode is a harness, not an OpenAI-compatible endpoint. Do not map it to `free-router` or
`openai_compatible`. Those routes describe provider HTTP behavior and price policy. OpenCode adds
an ACP session, tool loop, permission requests, and process lifecycle.

Reuse the existing OpenCode ACP transport and process containment where safe. Do not reuse its
repair `WorkerPort` result or its default `auto_approve = true`. An `OpenCodeReviewPort` must:

- implement `ReviewPort`;
- receive the same common review task and pinned checkout as Codex;
- deny mutating permissions;
- strip forge credentials;
- require a schema-valid final review result;
- verify that the checkout is unchanged; and
- classify availability with OpenCode-specific captured evidence.

Keep it disabled until an unattended, bounded, read-only smoke succeeds. This is not trivial enough
for Slice 3 because the current ACP adapter returns repair-shaped results and can auto-approve tool
permissions. Add it in Slice 4 after command/run identity and failure semantics are split.

Recommended initial enabled order is Codex, then OpenCode. Keep Grok declared but disabled until
its headless proof exists. Keep `free-router` separate and zero-only. If OpenCode's selected model
can charge money, it is not an allowed fallback under the current no-paid policy.

## Seams to keep, rename, and split

### Keep now

- One `[[shepherd.projects]]` repository row.
- Poll and later webhook as wake sources only.
- Pure `review_eligible` and SHA-exact checks.
- `ReviewPort`, `ReviewInvokeRequest`, `ReviewInvokeOutcome`, and `ReviewResult`.
- Review-specific ledger facts and the `active_run_id` fence.
- Shepherd as the only GitHub credential holder and writer.

### Rename only when touched

- `gate` to `review_gate`, with a compatibility alias, when the second gate kind lands.
- `ObserverIntent` to `ReviewIntent` only if publication makes the old name confusing. This is
  cosmetic and must not delay Slice 3.

### Split before Slice 4

- Review command from worker run attempt.
- `ReviewRunFailed` from `ReviewWorkerUnavailable`.
- Harness-neutral review task instructions from Codex command construction.
- Worker registry from ordered routing policy.
- Poll reconciliation from long-running worker execution, using the existing background/session
  ownership and ledger.

### Add in Slice 3

- A GitHub publication effect port.
- A restart-safe publication saga coordinator.
- A policy-versioned accepted key and GitHub marker.
- Required writer identity checks.

## Plan updates

The canonical status should say that Slices 0–2 are on main through PRs #244 and #247 and that
Slice 3 is next.

Before Slice 4 starts, update the canonical plan to state these implementation corrections:

- `harness_order` is a loaded ordered list, not map order.
- OpenCode is an explicit review worker kind after Codex.
- One review command can own several worker attempts.
- Failed and unavailable outcomes have different facts and routing behavior.
- GitHub idempotency markers include policy version.
- The required `review_profile` supplies common task instructions.
- Codex final-result extraction matches a captured real CLI stream.

## Decisions for Forrest

No new decision is needed to ship Slice 3. The locked COMMENT, checklist C, draft, identity, and
no-paid decisions determine the work.

For Slice 4, confirm these two defaults:

1. OpenCode is an explicit harness entry after Codex, not an alias for `free-router`.
2. OpenCode stays disabled unless its configured provider/model is proved no-cost or Forrest later
   approves a named paid policy.

The future set of gate kinds does not need a decision now. Add the typed gate dispatch only when a
second real predicate or trigger source arrives.
