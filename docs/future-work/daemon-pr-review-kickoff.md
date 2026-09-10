---
kind: plan
status: active
authority: advisory
domain: coding-control-plane
canonical_for: daemon-pr-review-kickoff
open_items: true
---

# Daemon-native pull-request review kickoff

**Status**: active. Forrest approved Sol pass 2 and the seven checklist defaults (2026-09-05).
Slices 0–3 are on main: Slices 0–1 via PR #244, Slice 2 via PR #247, and Slice 3 via PR #249
(2026-09-08). Codex-only homelab dogfood and cutover proof are active; later worker and webhook
slices remain open.

## Decision

Run PR review kickoff in the homelab Liberado daemon. Extend shepherd, the coding-task ledger, and
the session hub. Do not add an orchestrator or repository catalog.

One `[[shepherd.projects]]` row owns repair and review policy for each repository.
`ready_for_review` arms that exact head SHA. Liberado starts one review only after configured
checks succeed on the same SHA. Events only wake reconciliation; current GitHub API state is
authority. `review_requested` never arms or bypasses the gate.

An eligible review does not depend on a local branch. Shepherd fetches GitHub's exact
`refs/pull/<number>/head` plus the configured base branch into private `refs/liberado/reviews/`
refs, verifies the observed head SHA, and then creates the detached review worktree. Fetch failure
occurs before the durable command fence, so a later poll can retry it. The token stays with
shepherd and does not enter the Codex process.

The declared worker order is Grok Build, Codex, Antigravity (`agy`), Cursor local, and the
free-proxy/OpenRouter router. Only enabled workers enter admission. Fallback needs typed
unavailability. A bad review stops the route. The last route uses only models proved zero-price.

Liberado publishes an immutable PR review with event `COMMENT`. For code blockers, it also posts
a separate SHA-pinned checkbox issue comment and converts the still-matching PR to draft. It never
approves, requests changes, or merges. Human merge stays the hard gate.

## Pass 2 decisions

Accept all material Grok corrections:

- Fold review policy into `[[shepherd.projects]]`; no `[github_pr_review]` catalog. Set
  `cold_reviews = 0` while daemon review is active, but keep CI kickbacks.
- Make `review_requested` wake-only. The escape hatch is an audited CLI command.
- Query check runs and commit statuses for the armed SHA. Do not reuse `check_status`,
  `gh pr checks`, or approximate branch-run lookup.
- If an armed, in-flight, or reviewed tip changes, convert the matching PR state to draft and
  leave one short note. This permits a new GitHub `ready_for_review` transition.
- Classify Codex's captured usage-limit text; keep Grok disabled until headless proof; give
  `agy --print` about 30 minutes, not its five-minute default.
- Use checklist C: immutable COMMENT evidence plus a separate checkbox issue comment. Never read
  checkbox state.
- Add a review-specific operation and review ledger events. Do not overload repair
  `WorkerPort::start`, `WorkerRunResult`, `ReviewApproved`, or `ReviewRejected`.

No Grok disagreement is rejected. Each correction closes a real safety or liveness gap without
expanding the product boundary.

## Ownership

- Shepherd owns GitHub observation, eligibility, admission, fallback order, and PR side effects.
- The ledger owns durable PR, revision, command, review-run, publication, and draft facts.
- The session hub and coding pack own background execution and pinned workspaces.
- A review-specific port owns one harness invocation, not routing or GitHub policy.
- The free-proxy owns zero-price proof and failover inside its free candidate set.
- Workers get no GitHub credentials and cannot post, push, approve, draft, or merge.

Host the policy loop in the daemon. Keep `liberado shepherd` as an operator client and one-shot
diagnostic. Do not run the old writer and daemon writer for the same repository. Shadow mode does
not claim the lease. At cutover, exactly one controller owns each PR.

## Configuration

Use one repository row. Keep worker process settings in coder tuning and secrets outside TOML.

```toml
[[shepherd.auth]]
name = "github-homelab"
kind = "token"
token_ref = "LIBERADO_GITHUB_TOKEN"
webhook_secret_ref = "LIBERADO_GITHUB_WEBHOOK_SECRET"
expected_login = "ForrestThump"

[shepherd.review]
enabled = true
delivery = "poll"
poll_seconds = 120
reconcile_on_start = true
max_concurrent_reviews = 1
review_event = "COMMENT"
blocker_action = "draft"
on_synchronize = "draft_if_armed"
harness_order = ["codex", "open_code", "grok-build", "antigravity", "cursor-local", "free-router"]

[[shepherd.projects]]
name = "example"
repository = "OWNER/REPOSITORY"
coding_project = "example"
base_branch = "main"
profile = "coding-unattended"
review_profile = "coding-review-unattended"
controller = "liberado-shepherd"
auth = "github-homelab"
check_names = ["CI", "CRAP regression"]
cold_reviews = 0
gate = "ready_and_green_tip"

[tuning.coder.control_plane.review_workers.grok-build]
kind = "grok_build"
executable = "/home/box/.local/bin/grok"
enabled = false

[tuning.coder.control_plane.review_workers.codex]
kind = "codex"
executable = "/home/box/.local/bin/codex"
enabled = true

# Slice 4: OpenCode is a distinct harness after Codex (Forrest lock 2026-09-08).
# Not free-router / openai_compatible. Keep disabled until read-only smoke + no-cost/paid policy.
[tuning.coder.control_plane.review_workers.opencode]
kind = "open_code"
executable = "/home/box/.local/bin/opencode"
model = "PROVIDER/MODEL"
permission_mode = "deny_writes"
enabled = false

[tuning.coder.control_plane.review_workers.antigravity]
kind = "antigravity"
executable = "/home/box/.local/bin/agy"
output = "stream-json"
print_timeout = "30m"
enabled = true

[tuning.coder.control_plane.review_workers.cursor-local]
kind = "cursor_local"
executable = "/home/box/.local/bin/agent"
mode = "ask"
enabled = false

[tuning.coder.control_plane.review_workers.free-router]
kind = "openai_compatible"
base_url = "http://127.0.0.1:PORT/v1"
model = "auto"
pricing_policy = "zero_only"
enabled = true
```

This vocabulary is proposed. Validation rejects duplicate projects/workers, invalid
`OWNER/REPOSITORY`, unknown projects/adapters, literal secrets, two enabled controllers,
non-COMMENT review events, and non-draft blocker actions. This gate requires `cold_reviews = 0`
and nonempty `check_names` unless `all_reported_checks = true`. Require distinct
`cursor-local` and `cursor-cloud` IDs. Require `free-router` to be `zero_only`.

Start with a fine-grained PAT: Metadata read, Contents read, Pull requests read/write, Checks read,
and Commit statuses read. Resolve `GET /user` at boot and before publication. Fail closed if the
login differs from `expected_login`. Give the token only to shepherd. Prefer an App later for
several owners or organizations.

## Eligibility and events

```text
configured repository and controller lease held
AND PR is open, non-draft, and not a fork head
AND ready_for_review armed the current SHA, or an audited CLI command names it
AND every configured check succeeds on that exact SHA
AND no accepted result exists for repository + PR + SHA + policy_version
```

Query both SHA-pinned endpoints with pagination:

```text
GET /repos/{owner}/{repo}/commits/{sha}/check-runs?filter=latest&per_page=100
GET /repos/{owner}/{repo}/commits/{sha}/status
```

Match names against `check_run.name` and `StatusContext.context`. Only `SUCCESS` or `success`
passes unless config names another accepted conclusion. Missing, pending, neutral, skipped, stale,
cancelled, timed out, action-required, and startup-failure fail closed. Never call existing
`check_status`, `gh pr checks`, or branch-run fallback.

| Signal | Rule |
|---|---|
| `ready_for_review` | Persist an arm for its SHA, then re-read PR and checks. |
| completed check suite/run | Wake and coalesce by repository + SHA; re-query all checks. |
| `review_requested` | Wake only. It cannot arm or escape the gate. |
| `synchronize` | Never review the push. Supersede the old cycle. If this controller armed, ran, or published it, re-read the new head, draft it, and post one old-to-new SHA note. |
| `converted_to_draft` | Disarm and keep evidence. |
| ready `opened` | Do not infer an arm. Draft only if this controller owns open-as-draft. |
| `closed` | Close the task and cancel work. Never merge. |

The audited escape hatch is:

```text
liberado shepherd review --project <name> --pr <number> --sha <full-sha>
```

It uses a distinct command ID and the same open, non-draft, SHA, CI, controller, and publication
guards. `policy_version` is a committed contract version, not a retry counter.

Poll first. Reconcile bounded pages at boot and after API failure. Never infer a ready arm. A later
webhook only accelerates the same reconciler. It verifies raw-body HMAC, caps input, allowlists
repositories/events, persists delivery IDs, returns `202`, and does not replace polling.

## Review operation and evidence

Do not reuse `harness-eval::HarnessAdapter`. Add a review operation beside the repair port in
`coder-core`. Its request binds task/run IDs, repository, PR, base SHA, expected SHA, pinned
workspace, and schema. Its outcome is structured review, typed unavailable, failed, or cancelled.

The result contains reviewed SHA, summary, and findings with stable IDs, severity, blocker flag,
path, optional diff line, explanation, and optional check. Zero exit without valid schema fails.
A SHA mismatch is stale. Store capped raw output as an artifact with digest.

The workspace is read-only and pinned. Strip `GITHUB_TOKEN`, `GH_TOKEN`, `GH_HOST`,
`LIBERADO_GITHUB_TOKEN`, and forge credentials. Give the model a capped
`base_sha...head_sha` patch and diff stat. Cap or skip generated files.

Use review-specific facts:

```text
ReadyArmed
ReviewCommandIssued
ReviewWorkerUnavailable
ReviewRunFinished
ReviewPublished
ReviewChecklistPublished
ReviewDraftConverted
ReviewStale
```

Each fact carries its relevant command/run/worker/SHA/evidence IDs. Never map a clean COMMENT to
`ReviewApproved`; it is evidence, not merge approval.

### Harness rules

- **Grok:** first declared, but disabled and never probed per review. Enable only after a bounded
  unattended multi-turn, tool-capable, schema-valid run proves no pager/browser and captures a
  stable exhaustion signal.
- **Codex:** use `codex exec review` with read-only sandbox, JSON, schema, base/commit, and
  workspace. The first exhaustion fixture is exact versioned text
  `ERROR: You've hit your usage limit` plus retry time. Do not use HTTP 402. A tiny probe does
  not prove full-review quota.
- **Antigravity:** use print mode, stream JSON, schema, workspace, and about 30 minutes. Permission
  auto-denial is failure/auth, not quota. Never bypass permissions.
- **Cursor:** support local ask/plan only. Cursor cloud is separate and out of order. Capture a
  real plan-limit signal before enabling local fallback.
- **Free-router:** HTTP is authoritative; 402 belongs here. Only proved-zero catalog entries are
  eligible. Quota-then-pay needs known free remainder. Never hide a paid/cheap pin in this route.

Classifier order is structured signal, then exact versioned captured bytes, never exit code alone
or broad quota text. Mid-run exhaustion records cooldown and advances the same review key to the
next enabled worker in a fresh pinned checkout. Disabled workers start no process. Auth, malformed
output, permission failure, and normal model failure do not fall through.

## Publication saga

1. Re-read PR state; require reviewed SHA and non-draft.
2. Create or find one COMMENT review by
   `<!-- liberado-review:OWNER/REPO#N@SHA -->`, with `commit_id`.
3. Put valid in-diff findings inline. Render out-of-diff findings in the body so one invalid line
   cannot reject the whole review.
4. Record review ID, SHA, harness/run IDs, and submitting login. Never edit this review.
5. For blockers, create or find a separate checkbox issue comment by
   `<!-- liberado-checklist:OWNER/REPO#N@SHA -->`, with stable finding IDs.
6. Re-read head and convert to draft only if it still matches.

Cap both bodies. Never read checkbox state. If head changes before review, publish nothing. If it
changes after review, do not draft the new tip. Retry only the missing saga step.

## Idempotency

```text
review_key = repository_id + pull_request_number + head_sha + policy_version
```

Persist arms, revisions, exact checks, commands, admissions, cooldowns, runs, artifact digests,
publication/checklist IDs, login, draft results, cancellations, stale results, and escape-hatch
actor. Use stable command IDs, one writer, and the repository lease. Delivery ID is not a review
key. Late results remain evidence but cannot act.

## Smallest shippable slices

### Slice 0 — Freeze contract and live evidence

Freeze schema, ledger facts, permissions, prompt/diff caps, adapter commands, and classifier
fixtures. Grok and Cursor stay disabled until evidence exists.

Acceptance: invalid/mismatched SHA and malformed success fail; fixtures separate exhaustion,
rate-limit, auth, permission, timeout, and model failure; Codex uses exact usage-limit text, not
402; disabled Grok starts no process; config has no secret or hardcoded repository URL.

### Slice 1 — Polling observer and ledger only

Extend shepherd project rows, require `cold_reviews = 0`, keep CI kickbacks, and add bounded
multi-repo polling. Query SHA-exact checks. Record arms, tips, CI, draft, close, and synchronize
intent. Do not dispatch or write GitHub.

Acceptance: repositories do not share state; green SHA B cannot satisfy SHA A; only success passes;
old check helpers are not called; review requests only wake; ready-open does not arm; synchronize
records one draft/note intent and no review; restart is stable; CLI dry-run and daemon share pure
eligibility; fork heads fail; shadow holds no lease.

### Slice 2 — One pinned Codex review, no GitHub writes

Issue one command through the review port in a pinned checkout. Acquire the exact same-repository
GitHub PR head when its commit is not present locally; do not require a developer to fetch its
branch. Store structured result and new events.

Acceptance: crash recovery starts at most one run; all SHAs match; changed tip is stale; no forge
credentials or out-of-scope writes exist; malformed output fails; exact Codex exhaustion records
cooldown; no repair port/result/approval semantics are emitted.

### Slice 3 — COMMENT, checklist, and draft sagas

Publish one immutable COMMENT. For blockers, publish the separate checklist and draft the matching
PR. Execute synchronize-to-draft/note intent. Do not repair or merge. Publication uses a separate
GitHub effect port (not `ReviewPort`). Markers and accepted keys include `policy_version`.
`harness_order` is real config (Codex-only execution still). Codex stdout extraction accepts legacy
single-object JSON and documented JSONL `item.completed`/`agent_message` framing; a live capture
remains a residual when quota blocks collection.

Acceptance: only COMMENT is used; out-of-diff findings fall back to body; one checklist is created;
checkbox edits have no effect; restart resumes only missing steps; SHA/login guards precede every
write; synchronize creates at most one draft/note; no merge path exists; `expected_login` required
for writers; harnesses never receive GitHub credentials.

### Slice 4 — Enabled-worker fallback

Add proven adapters in `harness_order`. Forrest lock (2026-09-08): OpenCode is an explicit
`open_code` worker kind **after Codex** (not a free-router alias). Keep `enabled=false` until an
unattended read-only smoke succeeds and the selected provider/model is proved no-cost (or Forrest
approves a named paid policy). Also add Antigravity, Cursor-local, and free-router as enabled
only with captured evidence. Grok stays declared but disabled until headless proof. Do not wait
for Grok to ship Codex-first operation.

Acceptance: typed exhaustion advances once and normal failure stops; disabled adapters do not
start; cooldowns survive restart; OpenCode denies write permissions and strips forge credentials;
agy uses near 30 minutes; Cursor cloud is absent; free-router makes no paid or unknown-price
request.

### Slice 5 — Webhook accelerator

Add `POST /api/github/webhook` behind public HTTPS. Verify HMAC in constant time, cap and
allowlist input, persist delivery IDs, return `202`, and keep polling.

Acceptance: invalid/unknown/oversize input fails; duplicate, missing, and reordered events
converge; response never waits; polling repairs loss.

### Slice 6 — One-repository cutover and operations

Run shadow comparison, stop the matching Grok Bot listener, transfer the controller, and enable
daemon writes for one repository. Show armed SHA, CI blockers, enabled worker, cooldowns, last
review/checklist IDs, and draft result.

Acceptance: shadow has no writes/lease; one controller acts; a live cycle survives restart; a
blocker drafts; push does not review and restores draft; green CI plus a new ready transition makes
one review; a PR branch absent from the daemon host is fetched and reviewed; disabling admission
preserves history. One malformed or incompatible historical PR does not stop other PRs in the poll
page.

Every slice must pass preflight, mutation proof for claimed gates, the CRAP ratchet, and
`cargo metadata --locked`. Never raise a baseline or add a waiver.

## Migration off Grok Bot PR Reviewer

1. Add policy to the existing project row with `controller = "grok-bot"`,
   `cold_reviews = 0`, and daemon shadow observation. Keep current Grok Bot writes.
2. Compare arms, SHA-exact checks, and intended actions for several ready cycles.
3. Prove polling, remote-only PR acquisition, Codex COMMENT, restart, blocker draft, and
   synchronize draft in a test repo.
4. Stop the Grok Bot listener for the target repo. Confirm it cannot dispatch, comment, or draft.
5. Transfer to `liberado-shepherd` and enable writes for that row.
6. Use only the Liberado CLI escape hatch. Do not keep Grok Bot as a parallel manual controller.
7. After live restart, quota, stale-tip, and blocker cycles pass, remove the retired hook. Keep
   rollback config recoverable but never active in parallel.

Webhooks are not a cutover gate; polling is enough. Draft-on-open may remain a sibling only with
explicit ownership and no ability to arm review.

## Non-goals

- Auto-merge, approval, request-changes, automatic review repair, or branch-protection changes.
- Review on push or drafts, inferred arms, or fork heads in first slices.
- Another repository registry, orchestrator, workflow engine, or trigger DSL.
- Comparison adapters, repair results, or the fix-and-push cold-review prompt.
- Cursor cloud, paid emergency route, or cross-harness conversation continuity.
- Any hardcoded Liberado repository identity or URL.

## Forrest approval (locked 2026-09-05 ~10:28 PM CT)

ForrestThump approved Sol pass 2 (`6a189518`) and all checklist defaults:

1. Checklist C — immutable COMMENT + separate SHA-pinned checkbox issue comment.
2. Explicit nonempty `check_names` for Slices 1–3 (fail-closed).
3. Fine-grained PAT + `expected_login` for initial write slices.
4. One normal review per SHA + policy version; audited CLI for rare same-tip repeats.
5. Grok first in declared order but `enabled=false` until headless proof; Codex first enabled production reviewer.
6. Daemon-owned synchronize-to-draft + one short old→new SHA note.
7. Human merge remains the hard gate; no paid fallback without a later explicit policy.

Slices 0 and 1 need no further architecture decision. Implementation may proceed.
