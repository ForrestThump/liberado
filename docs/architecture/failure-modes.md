# How this system fails

**Status**: living. Distilled 2026-07-14 from twelve audits and findings in
[`../roadmap/archive/`](../roadmap/archive/), plus the live runs of 2026-07-13/14.

Twelve separate audits, spread over two weeks, kept finding **the same five bugs wearing different
clothes**. Reading all twelve teaches you the incidents. Reading this teaches you the pattern, which
is the part that will bite you again.

Every one of these shipped with a green test suite. Not one was found by reading the code.

---

## 1. The test was pointed at the wrong object

The single most expensive failure in this codebase, by a wide margin. The test passes. The test is
sincere. The test is testing something that isn't the thing.

| The test | What it exercised | What it was believed to cover |
|---|---|---|
| `ConversationStore` conformance suite | `JsonlStore` — which **nothing in production constructed** | the store chat actually uses |
| Coding-pack build tests | `ScriptedBackend` returning `Ok(Failed)` | a real backend, which fails with **`Err(NoChanges)`** |
| Zone-write-class tests | a grant with `ExecuteMcp` and **no `Write`** | that `Write` was enforced — it was never checked at all |
| The concurrency test | `#[tokio::test]` (single-threaded) + `join_all` (one task) | 50 concurrent appends. It ran them **strictly one at a time** |
| S7-c's "no false positives" test | scope lines containing **none of the domain's vocabulary** | that the linter doesn't cry wolf. It cried wolf on the first real contract |

Deleting `JsonlStore` and re-pointing its suite at `SessionStore` immediately exposed **two** live
defects the chat tests could never have found. The coding-pack double hid a seam that made the ask
unreachable from the case that most needed it. **Three separate defects, one root cause.**

**A4 (2026-07-23):** load-bearing `GoalSessionHub` behaviors (list, cancel, park→answer→resume,
durable rehydrate) are dualled against production `SessionStore` in
`crates/session-store/tests/hub_dual_store.rs` (plus the store-lens suite in `record_lens.rs`).
The in-memory `GoalSessionStore` remains for pack unit tests; it is no longer the *only* place
cancel/list/resume are proven. Tier-1 live conformance (`t1_conformance`) also drives the durable
store for HTTP-shaped goals paths — same doctrine, different surface.

> **The rule.** A test double that produces the shape you *expect* is worthless — it agrees with you.
> Ask: *what shape does reality actually produce?* If your double has never returned an `Err`, or
> never withheld a capability, or never used the words the domain is made of, it is decoration.
>
> **The check.** Before trusting a test, break the code it covers on purpose and watch it fail. If it
> doesn't fail, it never protected you. This is cheap and it has caught something every single time.
>
> **And do it mechanically.** `cargo mutants` breaks the code for you, one edit at a time, and reports
> which tests never noticed. It is the only tool that attacks this class directly, and it works:
>
> ```
> cargo mutants -p liberado-session --in-place --test-workspace=true
> ```
>
> First run against the session kernel (2026-07-14): **148 mutants, 66 caught, 39 missed** — 37% of
> the kernel's viable logic could be broken with no test noticing. Two of the misses were serious:
> `GoalSessionHub::cancel` could be replaced with a **no-op** and nothing failed (both surfaces offer a
> cancel button, and a cancel that silently does nothing is worse than no button — you believe the work
> stopped and walk away while it keeps running), and `list` could return an empty vec, which every
> session switcher reads.
>
> **Commit before running `--in-place`** — it edits your source and a crash leaves it mutated. (Ask me
> how I know.) `--test-workspace=true` matters too, or cross-crate tests are not credited and the miss
> count is pessimistic.

## 2. The guard that was off by default

A safety mechanism that is *opt-in* is a safety mechanism that is **off**, because the safe choice is
always more work and nobody takes it.

Zone declaration on an MCP was optional, and the model doc said so approvingly: *"zone-write-class
gating is opt-in per MCP, not a blanket restrictive default."* Consequence: **no MCP ever declared a
zone**, so `resolve_zone` returned `None` for every tool of every MCP, so *both* the zone-write-class
guard and the capability guard were **permanently inert**. A dispatch session granted `Read` and
explicitly denied `Write` wrote to the vault, live. Two layers of defence, both silently absent, for
months, looking exactly like protection.

> **The rule.** A guard's default must be *refuse*, and its absence must be *loud*. Declaring an MCP
> now means rating it, wiring it, **and saying what it touches** — and the daemon refuses to boot
> until you do (`config-loader/src/validation.rs`).
>
> **The smell.** Any sentence in a doc or comment that says a safety feature is "opt-in", "advisory",
> or "best-effort". Go and check whether anyone has opted in. Usually nobody has.

## 3. The narration outran the code

Comments, log lines, status strings and event messages that describe a system that does not exist.
These are worse than no message, because they *retire the worry*: a reader who sees
`retrying once with human guidance` stops asking whether it retried.

- `Progress { "retrying once with human guidance" }` — **followed by no retry.** The session failed
  and threw the answer away. Indistinguishable from working on every surface except the backend.
- `"goal session has already finished"` — returned for a **parked** session, the one thing it
  definitively has not done. The difference between *"start over"* and *"wait"*.
- A doc comment telling packs to check `Write(zone)` *"exactly as the MCP boundary does"* — **the MCP
  boundary did not do that.**
- `PROGRESS GUARD (fatal): … make the required edits or submit_report` — while **blocking both**
  `write_file` and `submit_report`. The model was ordered to act and denied every means of acting.
- `SessionStatus::Parked`'s doc claimed answering it "restarts the pack". Nothing consumed `Parked`.

> **The rule.** A message that asserts behaviour is a **claim**, and claims need the same scrutiny as
> code. When you change what a thing does, grep for what the system *says* about it.
>
> **The tell.** Narration written in the present tense about a future intention. If the code isn't
> there yet, the sentence must say so.

## 4. The machine check that could overrule the human

S7-c — the contract-coherence linter — shipped with three faults that compounded: it cried wolf
(§1), its redrafts spent the *human's* clarify budget, and on exhaustion it **failed the session**.
Net effect: a coding session died with `needs human review` **having never asked the human anything**.

The linter was wrong, and because it could terminate work by itself, "the linter is wrong" became
"the work is gone."

> **The rule.** An automated check may **defer to** a human. It may never **overrule** one. Its
> failure mode must be *"I could not decide, here is what I saw"* — never *"therefore this is over."*
> Machine checks catch what machines catch; the human is the backstop, and a backstop you can bypass
> is not one.

## 5. Write-only memory: the seam that only went one way

`PackContext` could `record_turn` but not read turns back. The store could `append_turn` but had no
`turns()`. Nobody noticed, because nothing had ever needed to *remember* — until resume did, and
then a session parked across a restart could only start over and ask you everything again.

Not a bug, exactly. An asymmetry nobody had reason to look at, which quietly bounded what the system
could ever do.

> **The smell.** An interface with a writer and no reader (or a reader and no writer). It usually
> means one direction was never imagined, not that it was rejected.

Related: the **path-traversal check that only went one way.** `Vault::to_relative` had correct
canonicalize+strip-prefix logic, but it was only called on watcher-delivered paths — not
tool-call-argument paths headed for `write`/`delete`/`move_note`. `Vault::write` passed a
tool-supplied `rel_path` straight to Turbovault with no `..`/absolute-path validation. The
`Vault::validate_rel_path` component-walk guard (2026-07-23) closed this by running before every
public entry point. The bug class: **a boundary that validates one entry path but not another** —
the code says "this is a secure boundary" and it's true for the path you looked at, but the other
path was never wired to the same check.

---

## The meta-lesson

**Every defect above passed the test suite and died on first contact with a running daemon.**

That is not an argument for fewer tests — the suite (1,175 and counting) catches regressions
constantly. It is an argument about *what unit tests can see*: they see the shapes you gave them.
The bugs live in the shapes you didn't.

So: **live-verify every slice against a real daemon before believing it.** Not as ceremony — as the
only step that has ever caught these. The pattern is so consistent that a slice which has not been
run should be treated as *unverified*, whatever the suite says. The commit messages in this repo say
"live-verified" for exactly this reason, and where they don't, be suspicious.

And when a live run does find something, **fix the test that should have caught it** — otherwise the
same class returns wearing different clothes, which is precisely what those twelve audits are.

> **The remedy, planned but not built**:
> [`../roadmap/live-conformance-suite.md`](../roadmap/live-conformance-suite.md). The live checks that
> caught all of this currently exist only as commands somebody typed once. The key realisation is that
> *most of them do not need a live model* — the ask seam, the parked session, the unenforced `Write`,
> the no-op `cancel` are all **plumbing**, and a real daemon with a `MockProvider` catches every one.
> So the valuable tier is fast, deterministic, and belongs in CI — not in an `#[ignore]`d graveyard
> nobody runs.
