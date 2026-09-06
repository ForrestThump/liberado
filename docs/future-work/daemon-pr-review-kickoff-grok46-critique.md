---
kind: critique
status: proposed
authority: advisory
domain: coding-control-plane
canonical_for: daemon-pr-review-kickoff-grok46-critique
open_items: true
---

# Grok 4.6 critique — daemon PR review kickoff (Sol pass 1)

**Status**: advisory sibling to
[`daemon-pr-review-kickoff.md`](daemon-pr-review-kickoff.md). Not a rewrite. Not
authorization to implement.
**Target**: Sol pass 1 @ `848ad7da` on `docs/daemon-pr-review-kickoff`.
**Also read**: current Grok Bot gate (`/home/box/.config/liberado/pr-review-gate.md`),
live harness order (`/home/box/.config/liberado/coding-agent-order.md`),
[`coding-worker-control-plane.md`](coding-worker-control-plane.md).

## Verdict

Keep Sol’s spine. Do not add a fourth orchestrator. Do not review on push. Do not
approve, request changes, or merge from this path.

Pass 2 must fix a small set of closed loops and false assumptions. The plan as
written would ship a second repository registry, reuse a CI function that treats
`SKIPPED` as green, probe a Grok CLI that still cannot run unattended, and
classify Codex exhaustion as HTTP `402` when the live CLI prints a usage-limit
sentence. Those are not nits.

## What is right (keep)

- One controller. Shepherd policy + coding-task ledger + session hub. No new
  kickoff daemon beside `liberado shepherd`.
- Eligibility is current GitHub state, not the webhook event name. Re-read before
  dispatch, before COMMENT, and before draft conversion.
- Arm on an explicit ready gesture. Green checks on the **exact** head SHA.
  `synchronize` does not start a review. That is the PR #243 lesson.
- `COMMENT` only. Human merge stays the hard gate.
- Typed unavailability falls through. A bad review does not.
- Do not reuse `harness-eval::HarnessAdapter`. Do not map goal `succeeded` to
  `ReviewApproved`. Do not reuse the cold-review prompt that commits and pushes.
- Do not send GitHub payloads through `POST /api/hooks/{name}`. That contract
  uses `X-Liberado-Hook-Secret` and a ten-minute in-memory cache. GitHub needs
  HMAC of the **raw** body, `X-GitHub-Delivery`, and a durable delivery id.
- Poll first. Webhook later as an accelerator. `hybrid` is the steady state, not
  the first production slice.
- Freeze adapter fixtures in Slice 0. Enable a harness only after an unattended
  smoke. Broad `contains("quota")` is forbidden.
- No hardcoded Liberado clone URL in product code. `OWNER/REPOSITORY` is a
  config value. Validation that already exists on `ShepherdProjectConfig`
  (`OWNER/REPOSITORY`, unique names, known `coding_project`) is the right seam.

## Must change

### 1. Kill the second repository registry

The example `[github_pr_review]` / `[[github_pr_review.repositories]]` table
contradicts the prose that says to extend `ShepherdProjectConfig`.

Two lists will drift. Repair kickbacks and review eligibility must share one
row.

Fold review fields into `[[shepherd.projects]]`. Keep shared GitHub auth as
`[[shepherd.auth]]` (or `[[github.auth]]`), not a parallel repo catalog. Keep
harness order in coder tuning as a **global** policy. A per-repo override is
allowed; it is not the default.

Required on each shepherded repo that uses this gate:

```toml
cold_reviews = 0                 # disable fix-and-push cold review
gate = "ready_and_green_tip"
review_event = "COMMENT"         # reject APPROVE / REQUEST_CHANGES
blocker_action = "draft"
review_profile = "coding-review-unattended"
controller = "liberado-shepherd" # or grok-bot during shadow
auth = "github-homelab"
check_names = ["CI", "CRAP regression"]
```

`cold_reviews = 0` is not optional. Today’s shepherd, on green CI, launches a
cold review that **fixes, commits, and pushes**
(`crates/cli/src/shepherd_cmd/prompts.rs`). That path and this path cannot both
be on. Keep CI kickbacks. Stop auto cold-review for daemon-native review repos.

Do not invent `[github_pr_review.repositories]`.

### 2. Close the ready-arm loop on `synchronize`

Sol disarms on `synchronize` and does not review the new tip. Correct.

GitHub will not emit `ready_for_review` again while the PR stays ready. After a
push, the author cannot mark ready without a draft transition. The plan leaves a
dead end: PR looks Ready, ledger is unarmed, no event can re-arm.

**Rule:** if this controller armed the tip, or has an in-flight review command,
or has published a review for the previous SHA, then `synchronize` converts the
still-matching PR to draft, disarms, and posts one short COMMENT or issue note
(`tip moved @ OLD_SHA → NEW_SHA; mark ready when CI is green`). Re-read head
before the draft call. If head already moved past `NEW_SHA`, skip the draft.

This is how “the author must mark the repaired tip ready again” becomes a
GitHub-native cycle. Leaving the PR ready is not that cycle.

Draft-on-open stays a sibling during migration, but `opened` with `draft=false`
must be specified: do **not** arm it. Convert to draft if this controller owns
the repo’s open-as-draft policy; otherwise refuse eligibility and record why.
Do not infer a missing `ready_for_review` from `draft == false` on reconcile.

### 3. Do not reuse `check_status` / `gh pr checks`

`crates/cli/src/shepherd_cmd.rs` `check_status` treats any conclusion other than
pending/fail as success. `SKIPPED` and `NEUTRAL` pass. `gh pr checks NUMBER` is
PR-scoped, not SHA-scoped. `latest_run` can fall back to an approximate branch
run. All three are illegal for this gate.

Query the armed SHA:

```text
GET /repos/{owner}/{repo}/commits/{sha}/check-runs?filter=latest&per_page=100
GET /repos/{owner}/{repo}/commits/{sha}/status
```

Match `check_names` against `check_run.name` **and** `StatusContext.context`.
Success is `SUCCESS` / `success` only, unless the repo lists extra
`accept_conclusions`. Missing, pending, `NEUTRAL`, `SKIPPED`, `STALE`,
`ACTION_REQUIRED`, `CANCELLED`, `TIMED_OUT`, `STARTUP_FAILURE` fail closed.

Empty `check_names` is already “all reported checks” in shepherd. For this
feature, reject empty unless `all_reported_checks = true` is explicit. Do not
read branch-protection required contexts in Slice 1: a fine-grained token often
cannot.

### 4. Usage-exhausted detection is not realistic yet

Live notes in `coding-agent-order.md` beat Sol’s matrix. Freeze these as
Slice 0 fixtures. Do not invent HTTP codes the CLI does not emit.

| Adapter | Installed here (2026-09-06) | Real unattended invocation | Exhaustion signal that exists | Do not do |
|---|---|---|---|---|
| Grok Build | `grok 1.0.13` | Unknown. `grok -p` is **single-turn** (no tool loop). `grok agent` still opens the “Open Grok Build” pager and hangs. `grok agent headless` is a WebSocket relay, not a proven CLI. `--output-format json\|streaming-json` and `--json-schema` exist on the main binary. | Not proven. 402 was seen on 2026-09-02, as a coordinator note, not a stable JSON field. Pager/login → `NotHeadless` / `AuthenticationRequired`. | Probe Grok on every review. Use `-p` for a multi-turn review. Classify stderr `quota` substrings. |
| Codex | `codex-cli 0.150.0` | `codex exec review --base <base> --commit <sha> --sandbox read-only --json --output-schema <file> -C <workspace>` (or `codex exec` with the same flags if `review` cannot pin the schema). | Live stderr: `ERROR: You've hit your usage limit` plus a retry timestamp (example: `3:05 AM UTC`). A tiny `codex exec` probe can return OK and a full review still dies mid-run. | Treat Codex exhaustion as HTTP `402`. `402` is OpenRouter insufficient credits, not ChatGPT Codex. |
| Antigravity | `agy 1.1.27` | `agy --print --output-format stream-json --json-schema <file-or-json> --print-timeout 30m --cwd <workspace>`. Default `--print-timeout` is **5m**. A Liberado review exceeds that. | No captured `RESOURCE_EXHAUSTED` fixture on this box. Headless auto-deny of permissions is `Failed` / `AuthenticationRequired` (seen 2026-09-05). | Call a 5-minute timeout `TemporarilyUnavailable`. Use `--dangerously-skip-permissions` to paper over auto-deny. |
| Cursor local | `agent 2026.09.02-c22c1a3` | `agent --print --output-format stream-json --mode ask --trust --workspace <path>`. `--mode ask` / `--mode plan` are read-only. Do not pass `--force` / `--yolo`. | No documented plan/usage JSON code in `--help`. Do not infer from a generic nonzero exit. Capture one real exhausted event before enabling fallback. | Share an adapter id with Cursor cloud agent. Cloud agents can write GitHub and would dual-control. |
| Free-proxy / OpenRouter | existing crate | HTTP is authoritative. `402` belongs **here**, plus `429`, timeout, and candidate-scoped failure. Catalog row must parse to zero. `QuotaThenPay` needs a known positive remainder (`crates/provider-free-proxy/src/quota.rs`). | Already implemented for model failover. Exhaustion of the free set is `Unavailable`, not a paid pin. | Put `deepseek/deepseek-v4-flash` in this slot. That is the live Grok Bot last resort, and it is not a proven zero-price catalog row. Automatic paid/cheap last resort stays a non-goal. |

Shared classifier rules:

1. Prefer structured event / HTTP status from **that** adapter.
2. Else exact, versioned byte strings from a captured fixture (Codex usage-limit
   sentence is the first one).
3. Never exit code alone. Never `contains("quota")`.
4. Mid-run exhaustion → `UsageExhausted`, durable cooldown with `retry_after` if
   parsed, same `review_key`, next **enabled** adapter, fresh pinned checkout.
5. Disabled / `NotHeadless` adapters are not probed. Declared order is Grok,
   Codex, agy, Cursor, free-router. Admission set is the enabled subset.
   Grok stays first **among enabled workers**. Config `enabled = false` until
   Slice 0 smoke passes. A durable `NotHeadless` with operator expiry is the
   same thing as disabled.

Suggested `AvailabilityReason` addition: `DisabledByPolicy`. Do not burn a
subprocess to learn what config already says.

### 5. Review port is not `WorkerPort::start`

`WorkerRunResult` carries commits, files changed, and tests. `ControlPlaneSupervisor`
exists to run repair workers. `ReviewApproved` in the ledger means today’s
shepherd cold-review **succeeded**, which may have pushed fixes.

Add a review operation beside the port (Sol’s second option). Do not overload
`start()`. Suggested shape is Sol’s `ReviewRunRequest` / `ReviewRunOutcome`,
returned into **new** ledger events, not `ReviewApproved`:

```text
ReadyArmed { sha }
ReviewCommandIssued { command_id, sha, policy_version }
WorkerUnavailable { worker_id, reason, retry_after }
ReviewRunFinished { run_id, worker_id, artifact_digest, blocking }
ReviewPublished { github_review_id, sha, login, event: "COMMENT" }
DraftConverted { sha }
ReviewStale { sha, reason }
```

A COMMENT with blockers is **not** `ReviewRejected` in the repair sense. A
COMMENT with no blockers is **not** `ReviewApproved`. Independent review does
not make the PR merge-ready. It only records findings.

Worker environment: strip `GITHUB_TOKEN`, `GH_TOKEN`, `GH_HOST`,
`LIBERADO_GITHUB_TOKEN`, and other harness API keys. The review workspace is
read-only. Shepherd is the only GitHub writer. Foreign CLIs do not get `gh`.

### 6. GitHub COMMENT saga needs diff-line fallback and an immutable body

`POST /repos/{owner}/{repo}/pulls/{pull_number}/reviews` with
`{ commit_id, event: "COMMENT", body, comments: [{ path, line, body }] }`.

If `line` is not in the diff for `commit_id`, GitHub rejects the **whole**
review. Put out-of-diff findings in the body only. Do not fail publication.

Cap body size (GitHub review body is not unbounded). Keep the machine marker
Sol named: `<!-- liberado-review:OWNER/REPO#N@SHA -->`.

Do not edit that review later to tick checkboxes. Editing mutates SHA-pinned
evidence. Working checklist, if any, is a **separate** issue comment with
`<!-- liberado-checklist:OWNER/REPO#N@SHA -->`. Liberado never reads checkbox
state. Draft + new ready is the cycle.

Draft conversion: `PUT /repos/{owner}/{repo}/pulls/{number}/convert_to_draft`
(or GraphQL `convertPullRequestToDraft`). Re-read `head.sha` and `draft` first.
Retry only the draft step. Do not post a second review.

Identity: `GET /user` at boot and before each publication. Compare with
`expected_login`. Fail closed on mismatch. Record `login` with the GitHub
review id. `COMMENT` is required while the token is a human login: GitHub
refuses `APPROVE` / `REQUEST_CHANGES` on your own PR.

### 7. Homelab ops that the plan under-specifies

- **Token**: fine-grained PAT for Slice 1–3 is acceptable. Scopes: Metadata
  read, Contents read, Pull requests read/write, Checks read, Commit statuses
  read. Webhook management is not required until Slice 5. A GitHub App is not
  a Slice 3 gate for a single-owner homelab.
- **Daemon vs `gh` auth store**: a service user will not see an interactive
  `gh auth login`. Inject `GH_TOKEN` from `token_ref` into the **shepherd**
  process only.
- **Webhook URL**: Slice 5 needs a public HTTPS endpoint. Local `:4201` is not
  that. Polling is the production path until Track B tunnel / a reverse proxy
  exists. Do not block Slice 1–4 on webhooks.
- **Receiver**: `POST /api/github/webhook`. Verify
  `X-Hub-Signature-256 = sha256=<hmac-sha256(raw_body, secret)>` in constant
  time. Bound body size (1–2 MiB is enough; we persist event, repo, PR, SHA,
  delivery id — not the full payload into the ledger). Allowlist repos and
  event types. Return `202` without waiting for a review. Persist
  `X-GitHub-Delivery`.
- **Fork heads**: first slices refuse PRs where `head.repo != base.repo`.
  Untrusted fork code in a review worktree is a later policy.
- **Shadow mode** must not claim the controller lease. Observation without
  `ControllerLeaseClaimed { controller: "liberado-shepherd" }`. Cutover is
  per repository, then the lease is per PR as already implemented.

## Disagree / challenge

- **“`review_requested` is an escape-hatch carrier.”** No. Requesting a human
  review must not dispatch a bot. Escape hatch is an audited command:
  `liberado shepherd review --project <name> --pr <n> --sha <sha>`.
  `review_requested` is a wake hint only, for compatibility with the current
  Grok Bot listener.
- **“Keep Grok first and skip it as `NotHeadless` each time.”** Skip means
  **do not start the process**. A per-review probe of a known-broken CLI is
  how you hang the queue. The 2026-09-03 note is still true on `grok 1.0.13`.
- **Live last resort is DeepSeek Flash, Sol’s last resort is zero-price
  free-proxy.** Agree with Sol for the daemon. Do not launder the paid/cheap
  pin through `free-router`. If Forrest wants that pin later, it is a named
  paid policy with operator approval — already a non-goal.
- **`WorkerPort` extension as the default.** The “or” in Sol’s sentence should
  resolve to a review-specific operation. Same crate (`coder-core`), different
  type.
- **Checklist in the COMMENT body as the recommended product.** Render
  `- [ ]` in a *separate* tracked issue comment if operators want GitHub task
  widgets. The review body is the immutable finding record.

## Open A/B questions — pick or challenge

### A — Checklist format

**Pick: neither A nor B as written. Use C.**

COMMENT review body: summary, SHA, harness/run ids, bullets for all findings,
machine marker. No task widgets in that body.

On blockers: one issue comment with GitHub checkboxes, SHA heading, stable
finding ids (`path + rule + title` hash, not `1,2,3`). Draft conversion is the
gate. Checkbox state is never eligibility.

Sol A puts checkboxes in the review body. Toggling them edits the review.
Sol B drops operator UX. C keeps both properties.

### B — Ready arming

**Pick A, with the synchronize-to-draft close from Must-change §2.**

`ready_for_review` arms. `review_requested` does not. Requiring both was a
Grok Bot listener limitation (`pr-review-gate.md` item 2), not a product
requirement. Dropping it is the point of moving kickoff into Liberado.

Challenge: `opened` as ready is not `ready_for_review`. Reconcile must not
seed arms unless migration policy says so once. Escape hatch is a CLI/API
command, not a review request.

### C — Required checks

**Pick A.** Explicit nonempty `check_names` per repository. Fail closed when a
named check is absent.

Do not take B in Slice 1–3. Branch-protection read needs permissions the PAT
may lack, and it still would not replace SHA-exact check-run queries.

Tighten vs current shepherd: empty list is not “all checks” unless
`all_reported_checks = true`.

### D — Authentication

**Pick A for Slice 1 and Slice 3.** Fine-grained token + `expected_login`.

Do not require a GitHub App before the first write. COMMENT + convert-to-draft
work with a human PAT on a single-owner homelab. Prefer an App later for a bot
identity and installation-scoped orgs. Record that as Slice 6+ , not a Slice 3
blocker.

### E — Review freshness

**Pick A.** One normal review per `repository + pr + sha + policy_version`.
Audited escape hatch for a repeat. Never B: a truncated run, a schema bump, or
a bad classifier will need a second look at the same tip.

`policy_version` is not a retry counter. Who may bump it: operator config
change, committed with the schema.

### F — Grok readiness

**Pick A, stricter.** Keep Grok first in **declared** order. Do not admit it
to the **enabled** set until Slice 0 records a bounded unattended success
(`--output-format json` or `streaming-json`, `--json-schema`, multi-turn tools,
no pager, no browser, classified exhaustion). Until then `enabled = false`.

Reject B. Blocking the queue on Grok recreates a single-vendor outage.

## Slice ordering

Sol’s 0→6 order is right: freeze → poll/ledger → one Codex review → COMMENT +
draft saga → fallback → webhook → cutover.

Insert or name these before calling it production:

| Gap | Where |
|---|---|
| Disable `cold_reviews` on native-review repos; keep kickbacks | Slice 1 config |
| Pure eligibility function shared by CLI `--dry-run` and daemon | Slice 1 (Sol already lists this; keep it) |
| SHA-exact check-run + status query; do not call `check_status` | Slice 1 |
| `synchronize` → draft if armed/in-flight | Slice 1 record; Slice 3 may perform the write |
| New ledger events; no `ReviewApproved` mapping | Slice 2 |
| Review port, not `WorkerPort::start`; strip GitHub env | Slice 2 |
| `codex exec review` + `--sandbox read-only` + `--output-schema` | Slice 2 |
| Out-of-diff line comments fall back to body | Slice 3 |
| Immutable review vs separate checklist comment | Slice 3 |
| Admission set vs declared order; no Grok probe | Slice 4 |
| Cursor-local only; cloud agent out of order | Slice 4 |
| Codex usage-limit fixture as first exhaustion corpus | Slice 0 and 4 |
| `agy --print-timeout` well above 5m | Slice 4 |
| Public HTTPS URL / tunnel before webhook | Slice 5 |
| Refuse fork heads | Slice 1–3 |
| Shadow mode does not claim lease | Slice 6 |
| Prompt/diff caps (files, bytes, generated-file skip) | Slice 0 schema + Slice 2 runner |

Do not wait for webhooks to retire Grok Bot. Polling plus Slice 3 writes is
enough for one repository.

## Token efficiency vs the tip storm

PR #243 was “new commit → full review”. This gate spends one review per ready
arm on a green SHA. That is the win.

The new way to waste tokens is fallback: five harnesses, or a Grok hang, or
`agy` killed at 5 minutes and restarted on Cursor, or check_run wakes that
re-dispatch because eligibility is not keyed.

Controls that must stay mechanical:

- `review_key = repo + pr + sha + policy_version` and one in-flight command.
- Coalesce `check_run.completed` by repo+SHA; always re-query the full set.
- Enabled adapters only.
- Durable cooldowns with source and expiry.
- `max_concurrent_reviews = 1`.
- Pin the worker to `git diff --stat` + capped patch of `base_sha...head_sha`,
  not a GitHub payload and not the whole tree.
- Never put credentials, webhook bodies, or CLI auth stores in model context.

A clean COMMENT does not start a second worker. A defect does not either.

## Config sketch (pass 2 vocabulary)

```toml
[[shepherd.auth]]
name = "github-homelab"
kind = "token"
token_ref = "LIBERADO_GITHUB_TOKEN"
webhook_secret_ref = "LIBERADO_GITHUB_WEBHOOK_SECRET"
expected_login = "ForrestThump"

[shepherd.review]
enabled = true
delivery = "poll"                 # hybrid only after Slice 5
poll_seconds = 120
reconcile_on_start = true
max_concurrent_reviews = 1
review_event = "COMMENT"
blocker_action = "draft"
on_synchronize = "draft_if_armed"
harness_order = ["grok-build", "codex", "antigravity", "cursor-local", "free-router"]

[[shepherd.projects]]
name = "example"
repository = "OWNER/REPOSITORY"   # never a cloned path, never a hardcoded URL
coding_project = "example"
base_branch = "main"
profile = "coding-unattended"     # CI kickbacks only
review_profile = "coding-review-unattended"
controller = "liberado-shepherd"
auth = "github-homelab"
check_names = ["CI", "CRAP regression"]
cold_reviews = 0
gate = "ready_and_green_tip"

[tuning.coder.control_plane.review_workers.grok-build]
kind = "grok_build"
executable = "/home/box/.local/bin/grok"
enabled = false                   # until Slice 0 smoke

[tuning.coder.control_plane.review_workers.codex]
kind = "codex"
executable = "/home/box/.local/bin/codex"
output = "jsonl"
sandbox = "read-only"
enabled = true

[tuning.coder.control_plane.review_workers.free-router]
kind = "openai_compatible"
base_url = "http://127.0.0.1:PORT/v1"
model = "auto"
pricing_policy = "zero_only"
```

Validation already sketched by Sol stays, plus: `cold_reviews == 0` when
`gate = "ready_and_green_tip"`; `cursor` is not a legal worker id (require
`cursor-local` or `cursor-cloud`); `free-router` without `zero_only` is
rejected; `review_event` other than `COMMENT` is rejected.

## Bottom line for Sol pass 2

The architecture is the right one. The control-plane slices already landed the
lease, the ledger, and the “do not build a fourth engine” rule. This feature is
that plane’s review policy, not a new product.

Pass 2 should absorb: one config row, SHA-exact checks, synchronize-to-draft,
review-specific port and events, realistic harness classifiers, Grok disabled
until proven, COMMENT saga that survives out-of-diff lines, and an escape hatch
that is not `review_requested`. After that, implement Slice 0–3 on one
configured repository. Leave webhooks and a five-harness router until one
Codex COMMENT cycle survives a daemon restart.
