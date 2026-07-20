# Chat latency + routing observability plan

**Status:** proposal (2026-07-20). Prompted by dogfooding the homelab daemon: interactive chat feels
slow because a single user turn that needs real work fans out across four-plus sequential LLM
roundtrips, each with extended thinking on a strong model (`deepseek-v4-pro`).

> **Landed 2026-07-20 (slice 1 — measurement + per-role config):**
> - Every inference call is recorded by a role-tagged `MeteredProvider`
>   (`crates/provider/src/latency.rs`) → `<data>/latency/events.jsonl` (role, model, wall_ms, TTFT,
>   tokens; correlation set at the chat turn + dispatch pack seams). Report:
>   `deploy/homelab/latency-report.sh` (p50/p95 per role).
> - **Per-role model tuning is now config-driven** (`[roles.main_agent|dispatcher|subagent]` in
>   topology.toml): provider, model slug, `temperature`, and `reasoning` level — edit + restart, no
>   rebuild. This is the enabling half of §3.1/§3.2 below.
> Still open in this doc: the short-circuit (§2), dispatcher-skip (§3.3), progress streaming (§2.4),
> stage timing + JSON-span logs (§4.2/§4.4), and actually choosing per-role models from the baseline.

This doc captures (1) the latency problem precisely, (2) the short-circuit idea, (3) other levers,
and (4) the **observability work that must land first** — we cannot claim a tuning win we cannot
measure, and today we measure nothing on this path.

---

## 1. The problem — the hop chain

Deployed mode is **face-agent mode** (`server/src/lib.rs:549`: `chat: face-agent mode`). For a chat
message that needs any real-world action, one user turn walks:

```
user prompt
  └─ FACE turn #1        LLM (+thinking)   main-agent/src/face.rs  — decides to call `delegate`
       └─ delegate       blocks on hub.await_terminal (face.rs:114) — chat turn stalls here
            └─ DISPATCHER LLM (+thinking)  dispatch-pack/src/lib.rs:170 — classify → DispatchDecision
                 └─ ORCHESTRATOR  LLM loop (+thinking, ≥1 turn, maybe parallel subagents)
                                             dispatch-pack/src/lib.rs:200 / orchestrator/src/lib.rs
            ◀─ compact report string
  └─ FACE turn #2        LLM (+thinking)   face.rs — phrases the report into the user-facing reply
final response
```

So the **critical path is ≥4 sequential model roundtrips**: face → dispatcher → orchestrator(≥1) →
face. Every one pays network + queue + prompt + **thinking** latency, and they cannot overlap because
each consumes the previous one's output. Two of the four (the dispatcher classification and face
turn #2) are "glue" calls that rarely need a strong thinking model at all.

Secondary pain: while `delegate` blocks (step 2), the chat SSE goes **quiet**. The subagent *is*
emitting `SessionEventKind::Progress` events (`dispatch-pack/src/lib.rs:189`) but those go to the
goal-session stream, not the chat surface — so the user stares at dead air, which makes the real
latency feel worse than it is.

---

## 2. The short-circuit idea (operator's) — bypass face turn #2

**Observation:** face turn #2 often adds little. When the subagent's report already *is* a good
answer ("Here are your 3 overdue tasks…", a looked-up figure, a confirmation), re-running the strong
face model just to rephrase it is a whole roundtrip of pure overhead.

**Proposal:** for turns where the subagent's report is directly presentable, **stream the report
straight into the chat conversation** as the assistant's reply and end the turn — skipping face turn
#2 entirely. Record the report as the assistant turn in the session transcript so continuity holds.
On the user's *next* message, the face agent resumes with that report already in its context — "as if
it had answered," when really it was the subagent's report.

**Not every turn short-circuits.** It depends on the nature of the work:

| Short-circuit (report → user directly) | Keep face turn #2 |
|---|---|
| Report is a self-contained answer/confirmation | Report is raw/structured and needs framing for the human |
| Single delegation, terminal `Succeeded` | Multiple delegations to synthesize, or partial/failed results to explain |
| No conversational stitching needed | Face must ask a follow-up, apologize, or weave into prior context |
| Report already reads as prose | `Clarify`/`Propose` dispositions (need the face to ask/route the human) |

**Who decides?** Cleanest is to make it an explicit signal on the subagent's result rather than a
guess: the orchestrator/report schema gains a `presentation` hint (`direct` | `needs_framing`), set
by the disposition type + a cheap check (terminal kind, single-vs-multi, length). Default to
`needs_framing` (today's behavior) so this is opt-in and safe. `Clarify`/`Propose` always route
through the face. This lives around `Report`/`Disposition` (`orchestrator`) and the `delegate` return
in `face.rs:120-151`.

**Win:** removes one strong-model thinking roundtrip from the critical path on a large fraction of
turns — likely the single biggest interactive-latency lever, and independent of model choice.

---

## 3. Other levers

1. **Per-role model tiering (the planned tuning).** Today the model is one global default
   (`OPENROUTER_MODEL`, set in compose to `deepseek/deepseek-v4-pro`). Give each role its own model:
   - **Dispatcher (router):** a fast, cheap, *non-thinking* model — classification into a small
     action set doesn't need extended reasoning.
   - **Face agent:** a fast model for interfacing/phrasing; strong reasoning isn't the bottleneck for
     "understand the human, call delegate, relay the answer."
   - **Orchestrator/subagent:** keep the strong model where the actual work + judgment happens.
   Requires per-role model config (new) — the `Dispatcher`, `Orchestrator`, and face `build_chat`
   construction sites (`server/src/lib.rs:444+`) each take a provider; wire distinct providers/models
   there. **Measure per-role before choosing** — see §4.

2. **Disable thinking where it earns nothing.** Even before swapping models, turn off reasoning
   tokens for the dispatcher classification and face turn #2. Big TTFT/latency cut for near-zero
   quality loss on glue calls.

3. **Skip the dispatcher LLM for confident/repeat routes.** The dispatcher already emits a
   `confidence` and has `DispatchTuning`. A heuristic/cache layer (goal-signature → last action) could
   bypass the classification call for obvious or repeated goals, deleting a whole roundtrip. Falls
   back to the LLM on low confidence.

4. **Stream subagent progress into the chat.** Surface the existing `Progress` events on the chat SSE
   during the `delegate` block. Doesn't reduce real latency but removes the dead-air gap — the
   cheapest perceived-latency win, and it also gives the user something to interrupt.

5. **Collapse face + dispatcher.** The face already decides intent when it calls `delegate`; the
   dispatcher then re-derives routing from scratch. Consider having the face emit a routing hint the
   dispatcher trusts (or fold classification into the face turn). Trades separation-of-concerns for a
   roundtrip; worth revisiting once §4 shows how much the dispatcher call actually costs.

6. **Speculation / overlap.** Kick off dispatcher classification the moment the face commits to
   delegating, and/or speculatively warm the most-likely route, so thinking overlaps instead of
   serializing. Higher complexity — do last, after the measured wins.

7. **Prompt caching.** Face and dispatcher share large stable prefixes (system prompt, catalog). Lean
   on provider prompt caching to cut per-call TTFT. Verify DeepSeek/OpenRouter cache behavior against
   the `claude-api` skill / provider docs before assuming it applies.

Rough ordering by (impact ÷ effort): **§2.4 progress streaming** and **§3.2 no-thinking-on-glue** are
quick; **§2 short-circuit** and **§3.1 model tiering** are the big structural wins; **§3.5/§3.6** are
later, measurement-gated.

---

## 4. Observability — build this FIRST (the actual prerequisite)

**We currently measure nothing on this path.** Concretely:

- **No wall-clock timing** anywhere hot — zero `Instant::now()/elapsed()` in `provider`,
  `dispatcher`, `orchestrator`, or the face bridge.
- **Spans don't emit durations.** `info_span!`s exist in dispatcher/orchestrator, but the serve-path
  subscriber (`cli/src/main.rs:22`) is plain `fmt()` — no `FmtSpan::CLOSE`, no JSON — so span timings
  never print and logs aren't machine-parseable.
- **Tokens parsed but dropped.** `Usage` (prompt/completion/total) is parsed in
  `provider/src/openai_compat.rs:311` and then discarded; `/api/status.token_usage_total` is `null`.
- **The dispatch journal is two bookends.** `.liberado/dispatches/<cid>.jsonl` holds only `start` and
  `disposition` records (`main-agent/src/dispatch_journal.rs`) — nothing between, and **nothing for
  the two face turns**, which are the roundtrips we most want to cut.

### Goal

One machine-readable record per **user prompt → final response**, joinable by `correlation_id`, with
per-hop wall time, token counts, and thinking-vs-tool split — so a before/after tuning comparison is
one query, not a stopwatch.

### Metrics to capture

- **End-to-end**: user prompt received → final response token.
- **Time-to-first-token** on the chat SSE (perceived responsiveness).
- **Per hop** (face#1, dispatcher, orchestrator per turn, face#2): wall time, model id, prompt /
  completion / reasoning tokens, and queue/wait time (hub start → pack run).
- **Roundtrip count** per turn (how many sequential LLM calls this turn actually took).

### Implementation (small, layered, low-risk)

1. **Instrument the one chokepoint — the provider `complete()` call.** Every roundtrip goes through
   it. Wrap it to measure wall time and emit a structured record: `{correlation_id, role, model,
   wall_ms, prompt_tokens, completion_tokens, reasoning_tokens}`. `role` (`face` | `dispatcher` |
   `orchestrator`) is passed down from the call site. This alone gives per-hop timing + tokens for
   free across the whole system.
2. **Add stage timing** with `Instant` at the seams: dispatch decision vs orchestrate in
   `dispatch-pack/src/lib.rs:170/200`; queue-wait + subagent wall in `DispatchBridge.delegate`
   (`face.rs`); and the two face turns in `chat_stream_core` (`server/src/api.rs`).
3. **Write a latency journal** — either extend the dispatch journal with a `turn` record per LLM call
   plus per-stage durations, or a sibling `.liberado/latency/<cid>.jsonl`. Must include the face
   turns (extend the journal's scope up into `chat_stream_core`, keyed by the same `correlation_id`).
4. **Make the serve subscriber analyzable** behind an env flag (e.g. `LIBERADO_LOG_FORMAT=json`):
   `.json().with_span_events(FmtSpan::CLOSE)` so existing spans emit durations and lines are
   `jq`-able. Keep human `fmt` as default.
5. **Aggregate `token_usage_total`** into `/api/status` (it's already `null` and wired for it) as a
   cheap always-on sanity signal.
6. **One analysis command** — a small script (e.g. `deploy/homelab/latency-report.sh` or
   `scripts/`) that reads the latency JSONL and prints p50/p95 per hop + roundtrip count. "Measure the
   gains" becomes one command against before/after runs.

### Sequencing

Land **§4.1–§4.3 + §4.6** first (measurement), capture a baseline from live dogfooding, *then* start
the §2/§3 tuning and compare against that baseline. Do not tune before the baseline exists.

---

## Anchors (as of this writing)

- Face agent + `delegate` bridge: `crates/main-agent/src/face.rs`
- Dispatch journal: `crates/main-agent/src/dispatch_journal.rs`
- Dispatch pack (dispatch → orchestrate): `crates/dispatch-pack/src/lib.rs`
- Dispatcher (classification LLM): `crates/dispatcher/src/lib.rs:113`
- Orchestrator (subagent loop): `crates/orchestrator/src/lib.rs:316`
- Provider completion + `Usage` parse: `crates/provider/src/openai_compat.rs:311`
- Chat SSE turn: `crates/server/src/api.rs` (`chat_stream_core`)
- Chat assembly / face-agent mode: `crates/server/src/lib.rs:444` (`build_chat`), `:549`
- Serve-path tracing init: `crates/cli/src/main.rs:22`
