---
kind: plan
status: active
authority: implementation
domain: product
canonical_for: implementation-backlog
open_items: true
---

# Backlog — implement in this order

Maintained 2026-08-11. This file is an ordered work queue. Take the first open, unblocked item in
the implementation order below. Do not choose a later item because it is easier or has fewer
dependencies. One item per PR.

If an item is blocked only by elapsed time, access to another repository, or an external service,
record the exact blocker in `current_unmerged_work.md` and take the next item. Do not skip a hard
code dependency.

## Implementation order

This list is authoritative. The bands below contain the full acceptance context; they do not define
a second priority order.

| Order | Item | Why now / dependency |
|---:|---|---|
| **1** | **F9 — cap concurrent background commands** | Safety first. PR #92 closed without merge; another unbounded build burst can exhaust the disk and invalidate every later run. |
| **2** | **0.1b — stage ship preflight output** | Fix the remaining deterministic gate defect before measuring the harness: format as an action, then compile, then report test and clippy failures together. |
| **3** | **D2 — configure model prices** | The baseline and later A1 report need real cost, not `unpriced`. |
| **4** | ~~**0.6 — emit the joined MVL and execution logs**~~ **Landed (PR #151).** | The assembly path and contracts landed in PRs #141 and #140. This instrument is the prerequisite for controlled comparisons. |
| **5** | **0.7 / C3 — publish the controlled cross-harness baseline** | One item, not two: instrument the pinned forks and compare fixed tasks only after 0.6. |
| **6** | **C5 — measure the completion gate** | Use the same baseline method to compare gate off/on before changing the default. |
| **7** | **0.9 — implement one evidence-selected cost lever** | Select from trace evidence. Tool-output offload is a hypothesis, not permission to skip measurement. |
| **8** | **A1 — read one day of deployed token-economics data** | Measure the existing production system before narrowing its catalogue. |
| **9** | **A2 — narrow the tool catalogue** | Blocked on A1. Change only what A1 supports. |
| **10** | ~~**B1 — give `ExecuteDirect` an explicit delivery destination**~~ **Landed.** | `ExecuteDirect` now carries `Delivery`; the orchestrator attaches the matching output contract. |
| **11** | **C1 — replace unrestricted shell git with a capability-visible library path** | Close the residual authority hole before expanding parallel coding. |
| **12** | **E4 — add directory enumeration in turbovault** | External prerequisite for the inbox layer. Record the upstream commit before continuing. |
| **13** | **E5 — stop the turbomcp SSE reconnect storm** | Restore useful homelab diagnostics before dogfooding the inbox path. |
| **14** | **F12 — give the vault watcher a positive scope** | Prevent unrelated note edits from dispatching work; this must precede E2. |
| **15** | **E2 — implement the inbox layer** | E3 is landed; start only after E4 and F12. |
| **16** | **C6 — add repo-map and context selection at the kernel/pack seam** | Large context lever; do it after the measured harness work so its effect can be isolated. |
| **17** | **C7 — expose one isolated parallel execution path** | Build on the proven worktree boundary and C1's safer git path. Do not expose both fan-out APIs at once. |
| **18** | **C4 — finish dedicated goal-view panes** | Useful surface work, but it does not block correctness, measurement, or unattended shipping. |

### Branch and integration rule

Do not branch every item from `main` by habit. Before creating a branch, write these four fields in
`current_unmerged_work.md`: **base commit**, **predecessor**, **shared files**, and **merge order**.

- If the item depends on or edits the same integration points as an unmerged predecessor, branch
  from that predecessor. Open it as a stacked PR against the predecessor, or wait for the
  predecessor to merge and rebase it onto `main` before opening.
- If the item is independent, branch from current `main`. Do not stack unrelated work.
- When a predecessor merges, rebase each dependent branch onto the new `main`, rerun its local
  gates, force-push with lease, and require fresh GitHub CI before merge.
- The PR body must name the base SHA, predecessor, shared files, and intended merge order. A branch
  with an undeclared overlap is not ready for review.

> ## Enforced — a PR missing any of these is closed without review
>
> Four rules. The first three are about tests; the fourth is about where the code lands.
>
> 1. **A "Still open" line** saying how you confirmed the item is not already done.
> 2. **A "Mutation evidence" section** with one entry *per behaviour you changed*, each pasting the
>    test that failed when you broke that one thing.
>
> **3. If you can drive the real code path, do not hand-build the intermediate.** Mutation evidence
>    proves your fixture is sensitive to a line; it does *not* prove the fixture models reality.
>    Three of the last four defects survived per-site mutation because the fixture agreed with the
>    implementation — hand-built events, a flag beside the macro, a prompt missing the block being
>    ordered. Before writing the assertion, answer: **would this fixture pass if the feature were
>    implemented the wrong way?** See [`delegation-failure-modes.md`](delegation-failure-modes.md).
>
> **4. Say where it goes, and why.** Every crate declares a role (`kernel`, `pack`, `surface`, …)
>    and CI enforces that `pack` depends on `kernel`, never the reverse. So domain code cannot leak
>    into the kernel — but **general machinery built inside a pack is invisible to that rule**, and
>    that is where duplication comes from: the next pack rebuilds it.
>
>    Before you write it, answer: **would a second pack need this?** Yes → kernel, with a thin
>    pack-side adapter. No → pack. State your answer in one line in the PR. The reference shape is
>    the completion gate: quorum logic in `liberado_session::completion_gate` (kernel), "what counts
>    as a build starting" in a coding-pack adapter.
>
>    Heuristic: **if you cannot describe the thing without saying "file", "git" or "compile", it is
>    pack work.** If you can, justify why it is in `coder-*`.
>
> **Do not edit this file in your PR.** Say which item you took in the PR body; status here is
> maintained on `main`. Three PRs each struck through their own row, two of them on adjacent lines
> of the same table, and every merge after the first hit a conflict in a file with no bearing on the
> code. Conflicts scale with PR volume, and this is the one file every PR would otherwise touch.
>
> This is not a style preference. Both are cheap for you to produce and cheap for a reviewer to
> check, which is exactly why they are the gate: a PR that lacks them costs a reviewer a full read
> to discover it was untrustworthy. Closing unread costs nothing.
>
> **Copy this into every PR body:**
>
> ````markdown
> ## Still open
> Confirmed by: <git log / grep / the code you read> — the item is not already implemented.
>
> ## Mutation evidence
> ### <behaviour 1> — <file:line>
> Broke it by: <the one-line change>
> ```
> test <name> ... FAILED
>   left: ...  right: ...
> ```
> ### <behaviour 2> — <file:line>
> ...one entry per behaviour changed...
>
> ## Where it goes
> kernel | pack — because a second pack would / would not need this.
>
> ## Integration
> Base SHA: <sha>
> Predecessor: <item or none>
> Shared files: <paths or none>
> Merge order: <after item or directly after main>
>
> ## Not done
> <any acceptance item you could not satisfy, and why — this is a pass, not a failure>
> ````

## Why the review rules exist

**1. Verify the item is still open before you start.** `git log` the area and read the code. Every
item below was checked on 2026-08-03, but this file goes stale the moment someone lands something.
A doc's status line is a claim, not evidence — a round-3 deliverable was specced against a header
that had been wrong for a day, and that cost a planning round. **If it turns out done, say so and
stop.** That is a useful PR comment, not a failure.

**2. Per-changed-behaviour mutation evidence in the PR body.** Not "tests pass" — for *each*
behaviour you changed, break that one thing, run the suite, and paste the failing test. If you
changed three call sites, that is three mutations, not one.

This is the rule that matters most, and it is why it is enforced above. Two recent PRs each changed
three code paths and tested two of them; in both cases reverting the third left the entire
108-suite workspace green. A test that cannot fail is worse than a missing one, because it tells the
next reader the case is covered.

**Run the mutation — do not reason about it.** Of the branches that stated a mutation result in a
doc comment without running it, more than half were wrong. Paste real output.

Also: **a fixture that cannot distinguish your implementation from the wrong one is not a test.**
Write down the wrong version you are excluding, then check your fixture would actually fail it.

## Gates, before every PR

```
cargo fmt --all --check
cargo test --workspace --no-fail-fast
cargo clippy --workspace --exclude liberado-webui --all-targets -- -D warnings
```

Where an item asks for a number, the number goes in the PR body — not a promise to measure later.
If it genuinely cannot be measured yet, say why. Two recent PRs did exactly that and were right to.

---

## Band 0 — the autonomous PR machine (active focus, set 2026-08-11)

The goal is a dispatched task coming back as a PR whose review is taste and scope, not repair.
This band gives the harness-specific rationale. The repo-wide implementation order at the top of
this file is authoritative and includes safety and measurement prerequisites from other bands.

The ordering principle, from evidence: **every measured improvement so far came from fixing a
defect, not from tuning a value.** Edit failure went 66–70% → 8% → 0% across PRs #106–#128, all
defect fixes. No knob has yet produced a measured gain. Knobs come after the instrument exists.

| # | What | Size | Where |
|---|---|---|---|
| **0.1** | ~~**Wire the ship preflight into the ACP dispatch path.**~~ **Landed (PR #134).** `PreflightRunner` + the ship profile landed in PR #74 and are reached via `session_pack::build` → `preflight_hook::run_ship_preflight`. `crates/acp-bridge/src/coding_run.rs` **does not use `session_pack`** — it names `CodingSessionPack` only in comments. Every ACP-dispatched run, which is every dogfood run since Paseo landed, skipped the ship bar. Most likely single reason each run needed hand-finishing. Acceptance: a dispatched run's trace shows preflight executing, and a run that fails it does not report success. | small–medium | `crates/acp-bridge/src/coding_run.rs` |
| **0.1b** | **Stage ship-preflight output.** Format as an action, then compile, then run test and clippy so the model receives the complete actionable failure set instead of stopping at the first required step. Preserve differential comparison against the base. | small–medium | `crates/coder-sandbox/src/preflight.rs`, `crates/coder-agent/` |
| **0.2** | ~~**A success report must be backed by a test run.**~~ **Landed (PR #131).** A run filed `succeeded` over seven failing tests because `cargo check` and `validate` both passed. Either the pipeline requires a test verifier, or the report step refuses a success claim no test run supports. Traps: a workspace with no tests must still pass; do not run the suite per tool call; pre-existing failures are not the agent's fault (see `preflight.rs`). | medium | `crates/coder-agent/src/verify_pipeline.rs` |
| **0.3** | ~~**An empty critic response must not discard a completed run.**~~ **Landed (PR #132).** A run finished its work, passed `validate`, and was filed `Failed` because a reviewer call returned empty content. An empty provider response is a transient fault in the *reviewer*, not a verdict on the change — retry or abstain, never throw the work away. | small | `crates/coder-agent/src/critic.rs` |
| **0.4** | ~~**Make one production coding-run assembly path.**~~ **Landed (PR #141).** `assemble_production_run` is the shared pack-owned path for `CodingSessionPack`, ACP and the headless runner. Mechanical rules bind all production entry points to it, while surface-owned trace provenance remains distinct. | medium | `crates/coder-agent/`, `crates/acp-bridge/`, `crates/coder-runner/` |
| **0.5** | ~~**Finish the two joined trace contracts and their conformance fixtures.**~~ **Landed (PR #140).** MVL reconstruction now requires exact request metadata, unambiguous call-ID joins, full snapshots after context changes, and paired attempt boundaries in the joined execution log. Production emission remains 0.6. | small–medium | `docs/spec/reference/`, `crates/test-support/` |
| **0.6** | ~~**Emit the joined logs from the common boundary.**~~ **Landed (PR #151).** Write append-and-flush JSONL from `executor` / provider request handling, then adapt coding outcomes to it. Pass the shared crash-survival and reconstruction fixtures. Do not add a second coding-only source of truth. | medium–large | `crates/executor/`, provider adapters, `crates/coder-agent/` |
| **0.7** | **Instrument pinned pi, Hermes and Deep Agents forks and publish the baseline.** Use the same user task, repository commit, model, provider, sampling settings and resource caps; keep each harness's native system prompt and tool schemas. Run repeats where cost permits. Report ship-gate/merge-ready rate, cost per accepted result, p50/p95, human repair and trace-linked failure classes. | large, external patch series + one report | forks, [`harness-study-2026-08.md`](harness-study-2026-08.md) |
| **0.8** | ~~**Productize cold review + one fix round.**~~ **Landed (PR #142).** Review requests are fresh, path- and excerpt-bound to the actual diff, retained findings get one fix round, and readiness requires post-fix verification. | medium | `Skills/cold-review-pr.md`, `crates/coder-agent/` |
| **0.9** | **Implement one evidence-selected cost lever.** Tool-output offload is the leading hypothesis because truncation spends context and loses data. Change only that mechanism, rerun the baseline, and retain it only if accepted-result cost or quality improves. | medium | `crates/coder-tools/`, [`harness-study-2026-08.md`](harness-study-2026-08.md) |

**One follow-up from 0.1 remains.** Provider construction now uses the same multi-tier
`liberado_config::config_dir()` resolution as bridge startup (PR #137). `run_preflight` is still
fail-fast on the first failing **required** step, so a `fmt` slip stops the bar before `test` runs
and the model hears only about formatting. Item 0.1b replaces that contract with staged reporting.

**The bar only runs if the config is loaded.** 0.1 gates on the declared `[[projects]]` entry that
contains the run's root, so a bridge that resolved no config dir has no project and no bar. That was
the state for the whole dogfood period — see the config-dir note in `CLAUDE.md`. The bridge now logs
its resolved config directory at startup; check that line before concluding the gate is broken.

**Not in this band, deliberately:** per-model knob profiles and the SQL tuning ledger
([`model-knob-profiles.md`](model-knob-profiles.md)). Correct to defer while we run only DeepSeek v4
pro/flash. Extract a knob when a measurement shows the constant is wrong — as PR #128 did for
per-tool argument matching — and add it to `config_literal_rules.rs` in the same PR. That guard
covers exactly one config type today, so nine of the ten known config shadows are invisible to it.

---

## Band A — token economics (the measured priority)

56% of all token spend is the orchestrator's ~11k base context re-sent every hop; the full
measurement is in [`token-economics-findings-2026-08.md`](token-economics-findings-2026-08.md).

**Landed:** #42 #43 #44 #46 #49 #54.

| # | What | Pointer |
|---|---|---|
| **A1** | **Deploy, wait a day, and report what the instruments say.** Still nothing read. Offered-vs-surviving MCP counts; dispatcher cache hit (should rise from 22.3%); subagent-vs-direct split of the 92.8%; total `repeat_calls`. `--json` makes it scriptable. **Measurement only.** | `liberado-cost --json`, `docker logs` |
| **A2** | **Narrow the tool catalog.** Blocked on A1. | blocked |
| **A3** | ~~**Check the face agent's prompt for the ordering shape.**~~ **Landed (PR #64).** | `crates/main-agent/` |

## Band B — correctness gaps

**Landed:** #45 #47 #51 #56 #57.

| # | What | Pointer |
|---|---|---|
| **B1** | ~~**`ExecuteDirect` gets no output contract**~~ **Landed.** `ExecuteDirect` carries `Delivery`. A research chat relay gets `relay_directive`; acting work stays short; vault delivery files the report. The old pin was `execute_direct_gets_no_output_contract_today`. | [`delegated-work-is-discarded-at-the-seam.md`](archive/delegated-work-is-discarded-at-the-seam.md) |

## Band E — homelab dogfood asks (2026-08-08)

Raised by an agent adding an hourly inbox-ingestion schedule on the homelab; each one had a live
workaround in `topology.toml` standing in for it. **#1, #2 and the cheap half of #4 landed** — this
is what is left. Full write-up and line refs: the report is reproduced in the PR that closed the
first three.

Every claim below was verified against `main` before being written down. One claim from the same
report was **not** reproducible and is recorded at the bottom so nobody re-opens it.

| # | What | Size | Pointer |
|---|---|---|---|
| **E1** | ~~**Per-schedule turn budget.**~~ **Landed (PR #86).** A schedule can set `max_turns`, which is carried through `budget_for(depth)`. | medium | `crates/orchestrator/src/lib.rs` (`budget_for`) |
| **E2** | **Implement the inbox layer.** Design settled 2026-08-08 (`inbox-spec.md` §14): two capture surfaces (pinned widget file + folder), compare-and-swap clearing, and a hybrid trigger where the flag routes ownership — unflagged notes belong to the schedule, `#now` notes to the watcher. Human-vs-agent attribution and content-hash idempotency already exist. E3 landed in PR #90; E4 and F12 must land first. | large | [`../spec/inbox-spec.md`](../spec/inbox-spec.md) §14 |
| **E3** | ~~**Watcher ignore list.**~~ **Landed (PR #90).** `inbox_ignore_globs` prevents the generic watcher from processing configured capture paths twice. It is a denylist, not the positive scope required by F12. | small | `crates/daemon/src/vault_source.rs` |
| **E4** | **turbovault cannot enumerate a directory** (turbovault repo, not this one). `query_frontmatter_sql` needs the `sql` feature compiled in; `advanced_search` takes `exclude_paths` but no positive path scope; `get_notes_info` needs paths you already have. So "process everything in this folder" is not expressible. Any one of: enable `sql` in the homelab image, add `path_prefix` to `advanced_search`, or add `list_notes(path)`. | medium | turbovault `crates/turbovault-tools/src/search_engine.rs` |
| **E5** | **SSE reconnect storm.** `turbomcp_http::transport` logs read-error → stream-ended → reconnect in a tight loop: ~93.5k occurrences in 24h, ~50/min while idle. Survivable, but it evicts real diagnostics under log rotation. Likely a turbomcp keepalive/EOF issue. | medium | turbomcp |

**Not a bug — do not re-open.** The same report proposed routing MCP vault writes through the
capability check, on the grounds that they bypass the zone model. They do not. `write_target`
resolves a three-state answer whose `Undeterminable` variant **refuses**, and it is called on both
paths (`executor/src/risk_gated.rs`, `server/src/lib.rs`); `turbovault` is declared with
`zone_from_arg`/`write_tools`, and no component grants `Write { Vault = "Briefs" }`. The observed
`Briefs/` write is real but has some other cause — the homelab's own `policy.toml`, or a writer
outside Liberado (turbovault holds an `rw` mount and ships its own git/batch/plugin crates). Chase
it with a daemon log showing whether a refusal fired, not with a code change.

## Band C — agentic coding: get to self-hosting

**The bar is concrete: run these PRs on our own coder instead of OpenCode.** That is more useful
than "parity with X" because it is pass/fail, it is dogfooding, and the benchmark is trivial — give
the same task to the same model in each harness and compare.

**Landed:** #52 #53 #58. Isolation (#58) was the gate on all concurrency; `dispatch_parallel` and a
concurrent `delegate` are now unblocked.

**What already exists**, so nobody rebuilds it: tools are `list_files`, `search_text`, `read_file`,
`write_file`, `edit_file`, `apply_patch`, `git_status`, `git_diff`, `run_command`, `validate`. The
completion gate (S1) with gatekeeper veto and fresh-reviewer quorum is built and default-**off**.
Goal sessions, park/resume, live gate votes, and `GET /api/goals/{id}/diff` all work.

**On the reference implementations.** Grok Build **is** checked out, at
`%LOCALAPPDATA%\Temp\opencode\grok-build` (commit `ed6d543`, ~94 MB). It is a **Rust** workspace,
so this is porting rather than translating — read it. OpenCode and Kimicode are not checked out;
clone them before citing them.

Caveat worth knowing: that path is a tool's temp directory. It can be wiped between sessions and is
not shared. If it is gone, re-clone rather than working from memory of it.

**Cite file and function in the PR, and say what you changed and why.** A straight port of a design
that assumes a different execution model is how you get a subtly broken loop, and it is hard to spot
in review.

Crates in there that map onto items below, so nobody reads the whole 94 MB:

| Their crate | Ours |
|---|---|
| `xai-gix-status`, and `gix` (gitoxide) as a workspace dep | **C1** — they do git through a *library*, not a shell. That is a better answer than allow-listing `git` in `CommandPolicy`: a library call is something the capability model can see, an allow-listed shell is a hole in it. |
| `xai-fast-worktree` | #58's `WorktreeWorkspace` — compare before extending it |
| `xai-codebase-graph` | Repo-map/context selection, which we have nothing equivalent to |
| `xai-hunk-tracker` | `edit_file` / `apply_patch` quality |
| `xai-grok-tools`, `xai-grok-tools-api` | Our ten-tool surface |
| `xai-grok-subagent-resolution` | **C6** — subagents on the isolation #58 unblocked |
| `xai-ratatui-inline`, `xai-ratatui-textarea` | **C4** — goal-view panes |
| `xai-grok-shell`, `xai-grok-shell-session-support` | `run_command`, and whether a persistent shell session is worth it |

| # | What | Pointer |
|---|---|---|
| **C1** | **The coder cannot commit.** ~~No branch/commit/push tool…~~ **Tools landed (#59) and exercised live** in the 2026-08-05 self-host dogfood (`git_branch` / `git_commit` / `git_push`, author `liberado@local`). Residual: empty `CommandPolicy` allow-list still means shell `git` is unrestricted when used via `run_command` — prefer library/`gix` long-term. | `crates/coder-tools/`, `crates/coder-core/` |
| **C2** | ~~**Run one real PR end to end and write up where it fell over.**~~ **Landed.** Session `01KZAJN9NMRR1THMWZM8ZSBV5P` produced [PR #69](https://github.com/ForrestThump/liberado/pull/69); PRs #70 and #71 closed its recorded production-path findings. | [`self-host-coding-dogfood-2026-08.md`](self-host-coding-dogfood-2026-08.md) |
| **C3** | **Controlled cross-harness baseline.** This is the same work as 0.7, not a second item. Follow 0.7's fixed-task, fixed-model, fixed-resource acceptance criteria and land one report. | 0.7 |
| **C4** | **Dedicated goal-view panes** — role timeline, gate panel, verifier panel. Gate votes stream live (#53) but render inline in the joined pane, so the streaming has nowhere good to land. | `crates/tui/` |
| **C5** | **Turn on the completion gate and measure it.** S1 is default-off pending S7 because it costs `1 + fresh_reviewers` model calls per attempt. With `liberado-cost --json` that price is now measurable — run a handful of tasks with it on and off. | `[coder.gate] enabled` |
| **C6** | **Repo map / context selection** — the biggest context lever, and we have nothing equivalent. **Split the seam on the way in**: "rank and select the relevant context for a goal" is general and belongs in the kernel — a research or vault pack wants exactly that — while "walk a source tree and build a symbol graph" is coding and belongs in `coder-*`. Build it whole inside the pack and the next pack rebuilds the ranking half. Read `xai-codebase-graph` first. **This item is both the highest-leverage coding work and the most likely duplication source**; get the seam right rather than fixing it later. | kernel + `crates/coder-*` |
| **C7** | **Use the isolation #58 unblocked.** `dispatch_parallel` is built and unreachable; `delegate` is synchronous. Scope one of them onto `WorktreeWorkspace` rather than both. **Placement check while you are there:** `WorktreeWorkspace` lives in `coder-sandbox` (pack), but "give a parallel worker an isolated workspace" is general and `dispatch_parallel` is kernel-side. If the orchestrator needs it, it is on the wrong side of the line — say so rather than reaching across. | `crates/orchestrator/`, `crates/coder-agent/` |

## Band F — harness observability and the delegation split (2026-08-09)

Six coding runs of one task (E3) produced zero files for four of them, then a complete, compiling,
tested implementation on the sixth: PR #90, the first agent-authored PR here. The model was never
the limit. Five harness defects were, and the fixes are on `main` (#91).

**The pattern behind almost all of them.** A config value parses, validates, and reaches nothing,
because a consumer hardcodes a literal instead of reading it. Seven instances are now known, and
each was invisible until someone ran the thing and asked why a setting had no effect:

| shadowed | consumer that hardcoded it |
|---|---|
| coder role model | PR #89 |
| `[coder.gate]` | PR #87 |
| `[coder.coder]` | PR #88 |
| `[coder.progress]` | `session_pack/build.rs` |
| `read_only_turn_limit` | `coder-runner` pinned 6 over the shared default |
| gate `enabled` | `coder-runner/src/main.rs:212` — still hardcoded `false`, not yet fixed |
| `trace_dir` | every production call site, so the trace facility had never written a file |

**F5 was built to make the eighth impossible** and is on `main` — a serde-driven test that walks
`CoderTuning`'s fields and fails when one does not reach `CoderRunConfig`. Note what it cannot
catch: it proves the value *arrives*, not that the consumer then reads it instead of a literal. The
gate row above is exactly that residue — `[coder.gate]` is plumbed and `coder-runner` still hardcodes
`enabled: false`.

### Routing: which model gets which item

**Read [`delegation-failure-modes.md`](delegation-failure-modes.md) first.** It is the authority
here, written from 15 delegated PRs (#33–#45) across three rounds, and its conclusion outranks any
routing table:

> **Spec accuracy is a bigger lever than model choice.**

Today's runs are an independent confirmation. DeepSeek landed E3 correctly — right file, right
function, right drop mechanism, Windows path normalization unprompted — from a prompt naming files
and functions. The same model, the same day, given a vaguer prompt, wrote
`fn job_is_build_like(..) -> bool { true }` with a comment justifying the constant. **Name the file
and the function, or expect a stub.**

With that said, the two failure modes differ enough to change how you *review*, which is what the
routing below is actually for:

- **Grok over-claims.** Ambitious code, usually correct, described as doing more than it does.
  Review budget goes on **checking claims**, and its tests are the least trustworthy artifact in the
  PR — #35 shipped an acceptance item with no test, #37 shipped one that could not fail. So give it
  work whose result you can *run*: if correctness is visible by executing the thing, an over-claim
  is caught in seconds instead of by auditing fixtures.
- **DeepSeek omits.** Narrower code, described accurately. Review budget goes on **finding gaps** —
  count the paths changed against the paths tested (#40 was three vs two). So give it work with
  exact pointers, where "what's missing" is enumerable from the spec.

**This round is not recorded.** The doc covers #33–#45. The later round (~#60–#73, written by
DeepSeek and Grok while Claude usage was exhausted, then reviewed and fixed) left almost nothing on
GitHub — those reviews happened in-session, and the PRs carry 0–1 comments each. Two consequences
worth fixing: the findings are lost, and **which model wrote which PR is not recorded anywhere**, so
none of the routing above can be checked against the largest sample we have. Going forward, name the
model in the PR body and append each round to the failure-modes doc.

**Landed:** F1 F2 F3 F4 — #96 (Grok), plus follow-ups `54fa0af` (trace CLI called every real session
id ambiguous), `a4f7b7b`, `a899eee`. F5 (`every_shared_field_survives_the_conversion_to_run_config`,
`coder-core/src/tuning.rs`) and F10 (`verifiers_for` now runs `cargo check`) are also on main.

F6, F7, F8, F11 and F13 are landed. F9's earlier PR #92 closed without merge, so F9 is open and is
first in the implementation order. F12 remains open.

| # | What | Size | → |
|---|---|---|---|
| **F6** | ~~**Preserve work on signal and use meaningful branch labels.**~~ **Landed (PR #143).** The headless runner races execution against termination, preserves dirty work, and derives the task label from the session or prompt. | small | `crates/coder-runner/` |
| **F7** | ~~**Reconcile orphaned parked sessions at daemon startup.**~~ **Landed (PR #144).** Startup keeps parks that a registered pack can resume and cancels store-only orphans after all packs register. | medium | `crates/session/`, `crates/server/` |
| **F8** | ~~**`ModelRequestSent` event.**~~ **Landed (PR #117).** Request-time events record the offered tools and the resolved system-prompt hash. The common joined-log emitter remains 0.6. | small | `crates/executor/` |
| **F11** | ~~**An unattended goal must not receive `AskHuman`.**~~ **Landed (PR #138).** `interactive: false` now narrows the effective grant at `goals_start`; accepted-payload tests bind the real call site. | medium | `crates/server/src/api/goals.rs` |
| **F13** | ~~**Apply shepherd review labels only after success.**~~ **Landed (PR #139).** Pending review state settles from the goal's terminal result, and the default cold-review budget is 60 turns. | small | `scripts/pr-shepherd.py` |
| **F12** | **The vault watcher reacts to every note you touch.** `react()` special-cases `proposals/` and its archive, then dispatches **every other vault change** to the default pool — so editing any note anywhere makes an agent decide what to do about it. `inbox_ignore_globs` (PR #90) does not fix this and is misleadingly named: it sits under `[tuning.capture]` but is applied on the *global* watch path, so it is a vault-wide denylist, not an Inbox scope. Approximating a scope with it means enumerating everything you do not want reacted to — unbounded, and it fails open on a typo'd pattern. What is wanted is a **positive** scope, which is already the design in [`inbox-spec.md`](../spec/inbox-spec.md) §14: the flag routes ownership, unflagged notes belong to the schedule and `#now` notes to the watcher. Until that lands, the watcher's remaining job after E3 excludes `Inbox/` is "everything else in the vault", which is the opposite of what anyone wants. | medium | DeepSeek |
| **F9** | **Cap concurrent background commands.** One build-like program at a time, two background jobs total, refused in band. A run launched nine concurrent `cargo` builds and filled a 476 GB disk. PR #92 closed without merge; verify the current command runner before reusing any of that branch. | medium | DeepSeek |

## Band D — breadth, low risk

**Landed:** #48 #50 #55.

| # | What | Pointer |
|---|---|---|
| **D1** | ~~**Promote `provenance_ratio` / `delegation_cost` from examples to subcommands.**~~ **Landed (PR #63).** | `crates/cost/` |
| **D2** | **`liberado-cost` prices nothing** — the box declares no `[[models]]` rates, so every report reads `unpriced`. Config-only; schema and doc already exist. | [`tuning.md`](../spec/reference/tuning.md) |

## Not available

- **§3b, reusing tool results.** Reserved. A read is safe to reuse, a write is not, and "call it
  again" is sometimes the point. Getting it wrong is a silent double-execution bug in the write path.
- **Anything that changes `crates/provider/src/latency.rs`'s journal shape** without updating
  `crates/cost/tests/journal_shape.rs` in the same PR. The two are a contract.
- **Prompt-wording changes to `relay_directive` / `DIRECT_INSTRUCTIONS`** without reading the seam
  doc first. Those strings encode findings that cost real debugging. **A2 above is a re-ordering,
  not a re-wording** — move the blocks, do not edit what they say.
- **A3 before A1.** Narrowing the catalog without the measurement is the single most expensive way
  to be wrong here, because a plausible-looking fix that addresses the wrong cause still ships and
  still looks like progress.
