# Backlog — pick from here, scope it yourself

Maintained 2026-08-03. **This is the direction; you choose the shape.** Take the highest item you
can do well, scope it, open a PR. One item per PR.

Items are ordered within each band. Bands matter more than positions inside them.

> ## Enforced — a PR missing either of these is closed without review
>
> 1. **A "Still open" line** saying how you confirmed the item is not already done.
> 2. **A "Mutation evidence" section** with one entry *per behaviour you changed*, each pasting the
>    test that failed when you broke that one thing.
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
> ## Not done
> <any acceptance item you could not satisfy, and why — this is a pass, not a failure>
> ````

## The two rules, in full

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
cargo test --workspace
cargo clippy --workspace --exclude liberado-webui --all-targets -- -D warnings
```

Where an item asks for a number, the number goes in the PR body — not a promise to measure later.
If it genuinely cannot be measured yet, say why. Two recent PRs did exactly that and were right to.

---

## Band A — token economics (the measured priority)

56% of all token spend is the orchestrator's ~11k base context re-sent every hop; the full
measurement is in [`token-economics-findings-2026-08.md`](token-economics-findings-2026-08.md).

| # | What | Pointer |
|---|---|---|
| **A1** | ~~Instrument the tool catalog.~~ **Done — PR #42.** | `crates/orchestrator/src/lib.rs` ~865 |
| **A2** | **Order the dispatcher prompt for cache reuse.** The varying goal is formatted *before* the stable MCP catalog, poisoning the prefix — dispatcher cache hit is 22.3% against ~76% elsewhere. Put stable content first. Then check the orchestrator's own prompt for the same shape; that one is 92.8% of spend. | `crates/dispatcher/src/lib.rs:240` |
| **A3** | **Report `repeat_calls` in `liberado-cost`.** Once PR #41 lands, the counter rides every filed report but nothing aggregates it. | `crates/cost/` |

## Band B — correctness and honesty gaps (each found and left open deliberately)

| # | What | Pointer |
|---|---|---|
| **B1** | **Hooks are not drain-gated.** `POST /api/hooks/{name}` can enqueue work *during* the 90s shutdown drain, which then dies with the process. The inventory records this as a stated hole. Gate it, or record why the daemon event loop makes it safe. | `crates/server/src/shutdown.rs` module docs |
| **B2** | **`ExecuteDirect` gets no output contract**, and `DIRECT_INSTRUCTIONS` asks for a *"concise, high-signal result"* — the shape of the seam bug. **Do not blanket-fix**: it carries no `Delivery`, so appending the relay directive would tell every cron and vault run to write documents. Needs a destination first. Read the doc before touching. | [`delegated-work-is-discarded-at-the-seam.md`](delegated-work-is-discarded-at-the-seam.md) |
| **B3** | **A goal session parked at shutdown records `Parked`, but nothing tells the human why.** A marker node on the transcript would turn "unanswered" into "the daemon restarted". | `crates/server/src/shutdown.rs` |
| **B4** | **`grace_secs` is 90s; delegating turns routinely exceed it.** Median delegating turn is 26k tokens over ~4 hops. Either raise the default or document the tradeoff where an operator will see it. | `crates/server/src/shutdown.rs`, `tuning.md` |

## Band C — agentic coding (S2 leftovers, then S3+)

Plan: [`coding-tui-plan.md`](coding-tui-plan.md). S1 landed, S2 partial.

| # | What | Pointer |
|---|---|---|
| **C1** | **`GET /api/goals/{id}/diff`** — does not exist. The goal surface can show file-changed events but not the diff itself. | `crates/server/src/api/goals.rs` |
| **C2** | **Gate votes reach the wire batched at attempt end, not live.** The kernel's `GateObserver` supports live emission; `CoderBackend::run` has no `SessionEvent` sender to plumb it through. Wiring one is the remaining half of "watch the quorum vote". | `crates/coder-*` |
| **C3** | **Dedicated goal-view panes** — role timeline, gate panel, verifier panel as separate widgets. Gate votes and file changes currently render inline in the joined pane. | `crates/tui/` |
| **C4** | **`WorktreeWorkspace` does not exist**, and its absence is the only thing preventing a workspace race: `dispatch_parallel` is built but unreachable, `delegate` is synchronous, the executor runs tools serially. **Isolation must land before any of those change** — [`agentic-loops.md`](../spec/architecture/agentic-loops.md) §Concurrency, rule 11. Large; scope a slice. | new |

## Band D — breadth, low risk, easy to close unmerged

| # | What | Pointer |
|---|---|---|
| **D1** | **External dependency audit.** Unused deps, duplication, version drift across every `Cargo.toml`. Goal is compile wall-clock, so measure before/after and put both numbers in the PR. | workspace |
| **D2** | **`liberado-cost` has no `--json` output.** Every consumer today is a human reading a table; a machine-readable mode makes the token work scriptable. | `crates/cost/src/report.rs` |
| **D3** | **`provenance_ratio` and `delegation_cost` are examples, not subcommands.** If they earn their keep, promote them. If they do not, say so. | `crates/cost/examples/` |
| **D4** | **Compaction tail copies still exist on disk.** Any *new* reader walking a raw leaf path must skip `Author::is_compaction_tail_copy()`. Audit existing readers for ones that do not. | `crates/conversation-store/` |
| **D5** | **Telegram has no `/help` for the commands it actually supports.** It gained `/stop` and scoped `/model`; the help text predates both. | `crates/server/src/telegram.rs` |

---

## Not available

- **§3b, reusing tool results.** Reserved. A read is safe to reuse, a write is not, and "call it
  again" is sometimes the point. Getting it wrong is a silent double-execution bug in the write path.
- **Anything that changes `crates/provider/src/latency.rs`'s journal shape** without updating
  `crates/cost/tests/journal_shape.rs` in the same PR. The two are a contract.
- **Prompt-wording changes to `relay_directive` / `DIRECT_INSTRUCTIONS`** without reading the seam
  doc first. Those strings encode findings that cost real debugging.
