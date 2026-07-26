# Applying the 2026 orchestration survey to Liberado

Source: `Learning/Agentic Orchestration Architectures - Research Report.md` (vault, 2026-07-26) — produced
by Liberado's own deep-research path, which is worth noting since it is also the first artifact that path
delivered end to end.

**Where we already match it.** The report's "Harness" pattern is a description of this system — persistent
vault memory, hooks, packs, sub-agents, cron — and its claim that production systems pair a harness with one
or two inner patterns is exactly Harness + Supervisor (dispatcher → orchestrator → subagent) + occasional
Swarm (`dispatch_parallel`). Three of its six reliability patterns are already implemented, and in two cases
more rigorously than it describes: **early stopping** is the doom-loop guard at threshold 3, matching its
"three approaches then escalate" almost verbatim; **human escalation** gates on confidence floor, consequence,
*and* zone write-class rather than its looser "blast-radius boundaries"; **circuit breaker** exists as
degraded-peer marking with a half-open TTL in `CapabilityCatalog`. Its sixth takeaway — that Gartner's
predicted 40% cancellation rate reflects quiet shelving when "nobody can diagnose why," not dramatic crashes
— is the argument for the observability work of 2026-07-25/26 (`guard=`/`verdict=` fields, the authority
decision line, `config explain`, the deploy smoke check).

**Where it disagrees with us, and where it is wrong to.** It rates **checkpoint-and-resume** its highest
reliability lever (a 10-step chain going 20% → 72%), and we have none: a `depth=deep` subagent that dies at
turn 28 of 30 loses everything. The wrap-up reserve is a partial substitute — it guarantees output at *budget
exhaustion* — but not against a crash or provider error. That is a real gap and it is narrower than the WIP
store we deliberately declined to build (2026-07-26): the report argues for failure recovery *within* a run,
not for machinery to routinely resume half-finished work between agents. Its **reliability-cliff arithmetic
does not transfer** to our research loop, however: `0.85^10` assumes dependent steps where any failure kills
the chain, whereas thirty ReAct search turns are largely independent gathering actions — five failures still
produced a good report, twice. Do not cut the research budget on the strength of that section; it applies to
the dispatch chain, not to gathering. Read its statistics unevenly, too: the Chen et al. and Cemri et al.
citations are checkable peer-reviewed work, while most of the concrete numbers (the 60–70% retry figure, the
cliff math, the six patterns) come from a single blog cited ~15 times and are presented with identical
confidence.

**What is actually worth doing.** First, **prompt caching**, which is entirely unclaimed: there is no
`cache_control` or cache accounting anywhere, and `Usage` carries only prompt/completion/total, so we cannot
even see whether the provider is caching for us. A `depth=deep` run resends the same system prompt and the
same four MCPs' full tool schemas on all thirty turns — the most cacheable workload we have. Measure before
optimising: surface the cache fields first, then chase prefix stability (`MultiMcpRuntime.runtimes` is a
`HashMap`, so tool order is arbitrary across restarts). Second, **executable verification** on report
submission — the report is blunt that an LLM checking its own output is "self-congratulation," and our
231-byte delivered artifact is that failure exactly: the subagent reported `Succeeded` and nothing checked.
A non-model assertion at delivery (length, structure) would have caught it before the vault did. Third,
**checkpointing for `depth=deep` only**, where the loss is largest. The broader tension the report surfaces
is worth holding onto: cost optimisation (routing, caching) and architectural layering (agents where one
would do) pull in opposite directions, and it endorses the former while warning against the latter — our
dispatcher is the endorsed routing pattern, not over-engineering, but the face-agent hop in front of it is
a third model call before any work begins.
