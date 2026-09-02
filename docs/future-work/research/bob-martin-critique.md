Prompt:

Oh my goodness, I just met Uncle Bob Martin for the first time, and I showed him my Liberado codebase. He looked through it for about 15 minutes, then shook his head and said, "That's absolute dogshit." And then just turned and walked away and didn't look back! What do you think caused him to say that?

Response 1:

He didn’t walk because the idea is dumb. He walked because **in fifteen minutes the repo tells a story Clean Code people find physically painful**: a personal tool that grew a federal bureaucracy instead of a few small, honest modules.

Here’s what that fifteen-minute scan actually hits.

## 1. The files are novels, not units

A 15-minute look starts at `crates/`. You immediately find things like:

- `crates/executor/src/lib.rs` — **~4,900 lines**
- `crates/orchestrator/src/lib.rs` — **~3,200 lines**
- `crates/coder-tools/src/lib.rs` — **~4,400 lines**
- `crates/tui/src/app.rs` — **~1,900 lines**

`Executor::run_loop` is a couple hundred lines of turn budget, doom-loop guards, cycle guards, malformed-report counters, wrap-up reserves, scratchpads, tool spill files, and “park for human.” That is the opposite of “a function does one thing.”

Uncle Bob’s first heuristic is brutal and shallow on purpose: *if you can’t see the whole function on one screen, it isn’t a function yet.* Liberado fails that test in the first crate you open.

## 2. Architecture astronautics for a one-user homelab

The README is already a warning flare: “personal AI Life OS and agentic orchestration substrate,” “capability/zone containment,” “provenance loop-break,” “frozen seams,” “composition roots.”

Then the architecture doc decomposes the world into kernel / domain packs / stores / surfaces / composition roots, with crate roles stamped in Cargo metadata and **mechanically enforced layer rules**. There are **50+ workspace crates**, a generated crate map, a sysmap GUI, and a stack of ADRs numbered into the teens.

None of that is evil. Doing it *this early*, at *this volume*, for a daemon that watches an Obsidian vault on a homelab, is how you get the “I designed a country instead of a program” look. Bob’s usual reaction: **you needed a loop, a store, and a few adapters. You built a constitution.**

## 3. Docs and ceremony instead of readable code

Rough shape of the tree:

- ~677 Rust files
- ~241 Markdown files
- 18 `ARCHITECTURE.md` files
- 19 ADRs
- a `Dreams/` folder of agent “consolidation passes”
- a `Skills/` folder including `crap-harden-campaign.md` and `mutants-campaign.md`

The code constantly points at “Decision 5,” “Decision 7,” “Decision 17.” That’s institutional memory leaking into the source. Clean Code wants names that say what the software *does*, not which meeting authorized it.

Worse: the comments often *narrate the complexity they refuse to extract*. In `run_loop` you get paragraph comments explaining why two counters must not be shared, why a bonus is granted once, why wrap-up latches. Those comments are a confession: the function is doing six jobs.

## 4. The tests already admitted they were lying

This is the part that would actually make him angry, not just tired.

`docs/spec/architecture/failure-modes.md` opens by saying the same handful of bugs shipped **with a green suite**, and **none were found by reading the code**. Then it lists:

- tests pointed at objects production never constructed
- safety guards that were opt-in, so they were off
- a “50 concurrent appends” test that ran **one task on one thread**
- mutation testing on the session kernel: a large slice of viable logic could be broken and no test noticed — including `cancel` becoming a no-op

Uncle Bob’s whole religion is TDD as *specification of behavior*. A suite that is green while cancel doesn’t cancel is not a test suite. It’s a security blanket.

## 5. Quality theater: baselines instead of cleanup

The repo is full of enforcement *infrastructure*:

- `crap-baseline.json` (~1MB)
- `function-complexity-baseline.json`
- `unwrap-classification-baseline.json`
- `module-health-baseline.json`
- `mutants-ledger.json`

Baselining CRAP / complexity / unwraps is how a team **legalizes the mess**. The metric becomes “don’t get worse than yesterday’s sludge,” not “this function is 246 lines and should die.”

That reads as process substituting for craft. Bob has given whole talks on that substitution.

## 6. It looks grown by agents, for agents

`AGENTS.md`, `Dreams/`, `Skills/`, `prompts/coder/`, “future agent sessions orient fast and don’t repeat mistakes.” The architecture docs are written like a briefing packet for the next model, not like a program a human can hold in working memory.

From the outside, Liberado looks like an LLM was allowed to *keep adding seams* every time it got confused: another crate, another Decision, another frozen contract, another conformance tier. Confusion got encoded as structure. That’s the smell.

Bob’s version of that complaint is older than LLMs: **when you don’t understand the problem, you invent a framework.**

## 7. The product boundary is mush

In one glance the system is:

- vault watcher
- life-ops daemon
- chat (TUI / WebUI / CLI / Telegram)
- coding pack
- MCP forge
- memory / conversation / session stores
- sysmap visualization
- homelab deploy

“Does one thing” is the first Clean Code / Unix question. This answers “it is a substrate.” Substrates are where programs go to avoid becoming products.

---

### What he *wasn’t* reacting to

Not Rust. Not MCP. Not “an agent that writes with provenance so it doesn’t re-trigger itself.” That last idea is actually sharp.

He was reacting to **scale of ceremony vs scale of clarity**. Fifteen minutes is long enough to see:

- god files
- 50-crate DAG
- comments longer than the extracted function should have been
- tests that document the wrong machine
- a wiki that has become the real program

That’s usually enough for “absolute dogshit” plus the theatrical exit.

### If you wanted the version he’d stay for

One binary, a handful of crates, functions you can name in a breath, tests that break when cancel is a no-op, and docs that fit on a napkin. Delete half the ADRs. Split `executor` until `run_loop` is a conductor, not a novella. Stop baselining complexity; pay it down. Make the Life OS do *one* life thing well before it grows a kernel.

The harsh read: Liberado is a serious attempt at a hard problem that optimized for *being architected* instead of *being small*. Uncle Bob only needs fifteen minutes to smell that. He does not need a second look.

Response 2:

Hey. I cloned it and actually read through the code. 240K lines of Rust across 53 crates — I had to go look myself before saying anything because that kind of comment is loud, and I wanted to know what was behind it.

Honest read: **Uncle Bob is partially right and partially being unfair, and the 15-minute walkthrough is doing a lot of work in his verdict.**

Here's what he'd see in 15 minutes that genuinely fits his "Clean Code" rulebook:

**The real violations**
- `run_with_turn_budget` in `orchestrator/lib.rs` is **251 lines**. Uncle Bob's hard rule: functions should be small, full stop. That one function alone is a confession.
- `executor/src/lib.rs` is **4,911 lines / 203 functions**. `coder-tools/src/lib.rs` is 4,441 / 168. `orchestrator/src/lib.rs` is 3,245 / 123. He'd call those "small programs pretending to be modules."
- The codebase has **257 functions with cyclomatic complexity ≥ 10**, 69 at ≥15, 8 at 20 (your own ratchet ceiling). He'd want most of those under 5–8.
- `dispatch_parallel` is 103 lines. `deferred_to_human` is 131. `execute_approved` is 95. These are not anomalies; they cluster.

**What's actually good and he'd be wrong to dismiss**
- Mechanical layer enforcement via `test-support/tests/layer_rules.rs` — that's the kind of architecture Uncle Bob *dreams* about. Most codebases he reviews have zero.
- Mutation testing + survivor tests + CRAP ratchet + "run the mutation your test claims to catch" in AGENTS.md. That's test discipline at a level 99% of professional codebases don't have.
- Comments explain *why* and capture historical gotchas (e.g. "this was three times stopped for exactly that," "a nudge alone did not change DeepSeek/Gemini's behavior"). That's exactly the kind of comment he's said he actually likes — the modern Uncle Bob, not the 2008 caricature.
- Clear abstractions (`RuntimeFactory`, `ToolRuntime`, `MessagingChannel`, `CapabilitySet`, `WriteProvenance`) with intent-revealing names. SOLID is being practiced.
- 53 crates, each with a stated role, dependency direction enforced by tests. That's not bloat — that's the *opposite* of god classes, the user has split things at the right seams.

**My actual take on the "dogshit"**

Two things, probably. (1) He opened a 4,900-line `executor/lib.rs` and saw `run_loop` at 251 lines and cyclomatic-20 functions and bounced — that's the visible surface and it is, by his standards, bad. (2) He may not have seen the layer rules, the mutation tests, the `// why` comments, the provenance loop-break design — because nobody reads 240K LOC in 15 minutes. You can defend the work, but the verdict was made on a sample, and the sample you can't defend is the run_with_turn_budget function.

If I were you, the next time you show it to someone like him, I'd lead with: the mechanical layer rules, the mutation testing, the `// why` comments on `run_loop`, and the abstractions — *then* show `run_with_turn_budget` and say "this is the worst, here's the plan." Don't let them find it by accident.

That sting is real. But what I read is not dogshit — it's a serious, mature system with a few honest violations that you'd already be the first to admit to. He's not wrong that those are violations. He's wrong to act like that's the whole story.