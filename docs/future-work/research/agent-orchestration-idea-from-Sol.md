Yes. That’s probably the architectural move that makes Liberado more useful before its own coding loop is “good enough.”

You don’t actually need Liberado to be the best coding harness. You can make it the control plane.

The split would be:

Liberado = scheduler + event system + task state + routing + supervision

and then:

Codex / Claude Code / Grok / MiniMax / Liberado-native agent = interchangeable workers

That lets you exploit subscription-backed harnesses where they’re cheapest, while still keeping your automation logic, repo policies, maintenance schedules, and retry/review workflow in one place.

The hard part is exactly what you identified: preserving task continuity across harness boundaries.

I would not try to preserve “the same chat context” literally. That will become brittle because each harness has different session formats, hidden state, tool semantics, token accounting, and persistence behavior.

Instead, make the unit of continuity an explicit task record owned by Liberado.

Something like:

task ID

repo + worktree

triggering event

objective

acceptance criteria

relevant commits / diff

prior worker

worker session ID, if resumable

execution log

current diagnosis

test/CI results

artifacts

status

next action


Then a worker interaction becomes:

task state → adapter → harness

and after execution:

harness result → normalized result → task state

If the harness supports real session continuation, great: store its session identifier and resume it.

If it doesn’t, Liberado can reconstruct enough context from the task record and repo state to launch a fresh process without losing the actual work.

That distinction is important.

Suppose Codex opens PR #83 and CI fails.

You do not fundamentally need to recover every hidden reasoning token from the original Codex session. You need to know:

> Task 341 was implementing X.
Branch is liberado/task-341.
Worker changed A/B/C.
Acceptance criteria are D/E/F.
CI now fails in test Y with this output.
Continue fixing the task; do not broaden scope.



That is enough for a capable coding agent to pick up where another one left off.

So the architecture I’d aim for is closer to a durable workflow engine than a multi-agent chat system.

A provider/harness adapter could expose a small interface conceptually like:

start(task, workspace) -> RunHandle

resume(run_handle, event) -> RunHandle

status(run_handle) -> Running | Waiting | Completed | Failed

cancel(run_handle)

collect(run_handle) -> RunResult

But resume() does not necessarily mean “append a message to the same LLM conversation.”

For Codex it might truly resume a session.

For Claude Code it might invoke a continuation mechanism.

For a CLI that has no stable session API, it could simply start another subprocess in the same worktree with a generated continuation prompt.

That way Liberado doesn't care.

And I think you should make repo state authoritative, not agent context.

The durable state is:

git branch + commits + test results + task ledger

The agent conversation is disposable execution state.

That also solves a lot of ugly failure modes. If an agent crashes, subscription quota resets, the provider dies, or you decide to switch from Grok to Codex midway through a task, you haven't lost the task.

You can do:

main receives commit
        ↓
maintenance policy evaluates
        ↓
create Task #341
        ↓
router chooses Codex
        ↓
Codex subprocess modifies worktree
        ↓
tests pass
        ↓
open PR
        ↓
GitHub CI fails
        ↓
CI webhook → Task #341 event
        ↓
resume Codex if possible
OR
dispatch fresh MiniMax worker
        ↓
worker sees task state + failure + existing branch
        ↓
fix
        ↓
push
        ↓
review worker
        ↓
merge candidate

That's substantially more interesting than “Liberado has a coding agent.”

It becomes:

> Liberado can operate coding agents.



And that lets you defer the extremely difficult problem of building an agent loop that beats Codex/Claude Code/Cursor.

You can still improve Liberado's native agent over time, but it becomes one backend among several rather than the foundation the entire product depends on.

There are a few things I'd be strict about from the beginning.

First, every worker gets a dedicated worktree/branch. Never make continuity depend on an agent remembering what it did.

Second, event history should be append-only. A task should accumulate structured events like:

TaskCreated
WorkerStarted
CommitProduced
TestsPassed
PullRequestOpened
CiFailed
WorkerResumed
ReviewRejected
WorkerResumed
CiPassed
ReviewApproved

Then your orchestration logic becomes deterministic around those events.

Third, distinguish task identity from worker-run identity.

A single task might have:

Task 341
  Run 1: Codex
  Run 2: Codex continuation
  Run 3: MiniMax recovery
  Run 4: Claude reviewer

That will make retries, escalation, and model comparison much cleaner.

Fourth, normalize worker outputs. Don't depend on parsing prose like “I think everything is fixed.”

Have the adapter return something approximating:

status
summary
commits
files_changed
tests_run
tests_passed
blocking_issue
recommended_next_action
external_session_id

You can derive some of that independently from Git and the test runner rather than trusting the model.

And fifth, don't make Liberado responsible for feeding every CI error interactively into an agent forever. Have explicit retry policies.

For example:

CI failure
  ↓
same worker gets one repair attempt
  ↓
fails again
  ↓
fresh reviewer diagnoses
  ↓
original worker gets diagnosis
  ↓
still fails
  ↓
escalate / human review

Otherwise autonomous maintenance jobs can turn into quota-burning infinite loops.

This design also answers your subscription problem in a nice way.

Your router can choose the execution backend according to whatever resources happen to be available:

Codex quota available → Codex CLI
MiniMax subscription available → MiniMax-backed worker
Grok allowance available → Grok harness
cheap API task → DeepSeek
all subscription pools exhausted → defer maintenance
critical CI break → pay API regardless

You don't even necessarily need one universal LLM API abstraction.

You need a universal coding-worker abstraction.

That is a much more useful boundary.

The interesting thing is that this could become the differentiating part of Liberado. There are already many coding harnesses competing on “how good is our agent loop?” Trying to outperform all of them directly is a brutal target.

A system that says:

> “Use whichever coding agent you already pay for. Liberado schedules, supervises, retries, routes, reviews, and maintains continuity across all of them.”



is a different proposition.

And it fits your current problem very naturally: you already have substantial inference available through subscriptions, but those subscriptions are fragmented behind different harnesses. Liberado could turn that fragmentation into a resource pool instead of forcing you to pay API rates just because its own agent needs an OpenAI-style endpoint.

I think I would prioritize this ahead of making Liberado's native coding agent dramatically smarter. The native agent can improve incrementally once the orchestration substrate exists.
