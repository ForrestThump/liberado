Doom loops in agentic scaffolding are primarily a control-flow problem, not a model-quality problem. The agent lacks reliable stop conditions, keeps seeing ambiguous or non-updating tool results, loses track of what it has already done, or is allowed to replan and retry without bounds. The decisive fix is architectural: guardrails must live in the harness that owns the loop, not in the model's instructions — a guard that lives inside the agent's prompt is a suggestion; a circuit breaker in the control flow is a law  (BuildMVPFast) . The best-established defenses are hard budgets, explicit termination criteria, external state, repeat-call detection, and a supervisory layer distinct from the executor.
This is not just practitioner folklore. The MAST study (Cemri et al., 2025) analyzed 1,600+ execution traces across seven multi-agent frameworks and found that failures cluster into system-design issues, inter-agent misalignment, and task-verification gaps  (arxiv) — and that improving robustness requires better orchestration, not just larger models  (Medium) .
What causes them
Ambiguous or soft-failing tool output. The agent re-calls the same tool with the same args because the result never changed its state — it gets stuck repeating a tool in response to ambiguous outputs or soft failures  (FixBrokenAIApps) .
Missing exit conditions. The agent has no clear definition of done and no rule for failure handling. One framing is that a loop needs three exit mechanisms: a hard iteration cap, a tool-call repetition detector, and a domain-aware completion check  (AIQnAHUB) .
Memory/state loss (distinct from context rot). On long tasks, early observations scroll out of the window and the agent effectively forgets what it already did and redoes work  (Meritshot) . This is a memory-architecture failure, not just context dilution.
Context rot. Even within the window, accumulated steps and errors bury the original constraints.
Unbounded replan/recovery. The agent keeps trying new approaches to a failing step with no bound on how many alternatives it attempts, no formal diagnosis of why the first failed, and no escalation protocol  (arxiv) .
Multi-agent deadlock and error amplification. Agents wait on each other or spawn more of themselves. Early versions of Anthropic's own research system made errors like spawning 50 subagents for simple queries and distracting each other with excessive updates  (Anthropic) . Uncoordinated multi-agent systems can amplify errors up to 17×, while centralized architectures with a validation bottleneck contain amplification to roughly 4.4×  (arxiv) .
What works best
Hard caps enforced by the runner, not the model: max steps, max tool calls, max retries, wall-clock timeout, and a spend cap. The dollar cap is the one people add last and regret skipping first — a stuck overnight run quietly spends real money per turn  (BuildMVPFast) .
Explicit termination criteria defined before the run: success, safe partial completion, and escalation triggers.
Repeat-call guards that block identical calls or identical failing arguments past a small threshold and escalate instead of retrying.
External working memory — a structured scratchpad recording completed steps and current state, so progress survives context turnover.
Structured tool contracts. Tools should return machine-readable failures with explicit handling for every non-success code, and use strict data models with unambiguous parameters (e.g. user_id, not user) so results actually update agent state.
State-machine or graph orchestration that keeps execution in named states with typed transitions and bounded retries, instead of letting the model freestyle control flow.
A separate judge/supervisor. Don't let the executor grade itself. Anthropic notes that a second model screening the first tends to perform better than one call handling both the guardrail and the core response  (Anthropic) . A cheap evaluator node running every few steps can emit a hard TERMINATE/CONTINUE signal to catch oscillation the executor won't self-report.
Human checkpoints before irreversible actions — deletes, payments, production deploys — pausing for human review before actions that can't be easily undone  (Naitive) .
A robust operating pattern
Start in planning mode.
Execute one bounded step.
Validate the result with a structured check against external state.
If the same failure repeats, pivot or reduce scope rather than re-attempting.
If the retry budget is exhausted, escalate to human review or return a partial result.
The "drop a gear" heuristic — when a step looks unstable or out of distribution, retreat to simpler subgoals instead of hammering the same action — is a sensible practitioner pattern rather than a formally validated technique, but it aligns with the finding that unbounded ad-hoc recovery is a structural weakness of naive agent loops  (arxiv) .
Highest-ROI implementation order
Add step, tool-call, wall-clock, and spend caps in the harness first — cheapest, catches the worst runaway cost.
Make tool outputs explicit and non-ambiguous with structured error schemas.
Add a loop detector keyed on repeated calls, repeated failures, or no state change.
Add external working memory so progress persists across context turnover.
Move from ad-hoc loops to a state machine or graph with typed state and bounded retries.
Add a separate supervisory/evaluator layer and human checkpoints for irreversible actions.
The core lesson is unchanged: you don't solve doom loops by hoping the model becomes more obedient. You solve them by making looping either impossible or cheaply stoppable at the orchestration layer — and by putting the stop authority in code that the model cannot talk its way past.