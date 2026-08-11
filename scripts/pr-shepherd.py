#!/usr/bin/env python3
"""PR shepherd — drives agent-authored PRs to either "ready for human" or "blocked".

The gate is **differential, not absolute**. "CI must be green" conflates two facts that need
opposite responses:

    you broke something      -> a regression, must block
    it was already broken    -> not yours, must not block

Absolute green cannot tell them apart, so it locks out the entire class of work that *fixes*
a red base — including "make CI green", which is exactly the task you most want an agent to
be able to take. Instead we record the base's failing set and compare:

    base 5 failures, PR 0          -> strictly better  -> proceed
    base 5 failures, PR the same 5 -> no regression    -> proceed
    base 5 failures, PR those + 1  -> new failure      -> kick back, naming only the new one

Failures are tracked by *identity* (platform + test name), never by count. A count can stay
flat while one test starts failing and another stops, which is a regression that a numeric
check waves through.

Naming only the new failures is load-bearing. An agent told "CI is red, fix it" when the base
was already red goes wandering into unrelated repairs — that is how a one-feature PR grows
into eight. The message has to do the attribution for it.

Goals are started through the daemon's POST /api/goals, NOT the headless
`liberado-coder-run task run`. Only the daemon path runs the session pack, and the ship
preflight gate + project authorization live there.

State lives in GitHub labels: durable across restarts, visible in the PR UI, correctable by
hand — all three matter when this runs unattended overnight.

Usage:
    python scripts/pr-shepherd.py --self-test        # verify the log parser, touch nothing
    python scripts/pr-shepherd.py --dry-run --once   # decide and log, spawn nothing
    python scripts/pr-shepherd.py --once
    python scripts/pr-shepherd.py --watch
    python scripts/pr-shepherd.py --seed tasks.txt
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DAEMON = os.environ.get("LIBERADO_SERVER", "http://localhost:4201")
PROJECT = os.environ.get("SHEPHERD_PROJECT", "liberado")
BASE_BRANCH = os.environ.get("SHEPHERD_BASE", "main")

STATE_DIR = REPO_ROOT / ".liberado" / "shepherd"
LOG_PATH = STATE_DIR / "events.jsonl"
BASELINE_DIR = STATE_DIR / "baselines"

# A failing job is re-run once before we believe it. The fan-out test found earlier passed
# alone and failed under parallel execution; without this, one flake eats every kickback.
MAX_KICKBACKS = int(os.environ.get("SHEPHERD_MAX_KICKBACKS", "2"))
COLD_REVIEWS = int(os.environ.get("SHEPHERD_COLD_REVIEWS", "2"))
# Cold review needs fetch → diff → classify; 30 turns died mid-review on a 10-file PR (F13).
COLD_REVIEW_MAX_TURNS = int(os.environ.get("SHEPHERD_COLD_REVIEW_MAX_TURNS", "60"))

# Each goal's preflight runs the workspace suite (~20 min here) plus clippy, in its own
# worktree with its own target/. Eight at once is eight full builds fighting for CPU and disk.
MAX_CONCURRENT = int(os.environ.get("SHEPHERD_MAX_CONCURRENT", "2"))
POLL_SECONDS = int(os.environ.get("SHEPHERD_POLL_SECONDS", "120"))

L_RERUN = "shepherd:ci-rerun"
L_BLOCKED = "shepherd:blocked"
L_READY = "shepherd:ready"
L_KICKBACK = "shepherd:kickback-{}"
L_REVIEW = "shepherd:review-{}"

# Durable map of cold-review goals started but not yet labeled (F13). Label only on success.
PENDING_REVIEW_DIR = STATE_DIR / "pending_reviews"
_LIVE_GOAL_STATUSES = frozenset({"running", "pending", "starting", "active", "parked"})

# `test (ubuntu-latest)\tTests (…)\t2026-…Z test some::name ... FAILED`
_TEST_FAILED = re.compile(r"\btest\s+(\S+)\s+\.\.\.\s+FAILED\b")
# Compile/lint failures carry no test name; fall back to the step so they still register.
_STEP_FAILED = re.compile(r"\b(error(\[[A-Z]\d+\])?:|error: could not compile)")


# ── plumbing ──────────────────────────────────────────────────────────────────


def log(event: str, **fields) -> None:
    """Append one JSONL event — including the no-ops.

    Running unattended, silence is indistinguishable from a crash, so every decision gets a
    line whether or not it did anything.
    """
    record = {"ts": datetime.now(timezone.utc).isoformat(), "event": event, **fields}
    LOG_PATH.parent.mkdir(parents=True, exist_ok=True)
    with LOG_PATH.open("a", encoding="utf-8") as fh:
        fh.write(json.dumps(record, default=str) + "\n")
    tail = " ".join(f"{k}={v}" for k, v in fields.items())
    print(f"[{record['ts'][11:19]}] {event}: {tail}")


def gh(*args: str, check: bool = True) -> str:
    proc = subprocess.run(
        ["gh", *args], cwd=REPO_ROOT, capture_output=True, text=True, encoding="utf-8"
    )
    if check and proc.returncode != 0:
        raise RuntimeError(f"gh {' '.join(args)} failed: {proc.stderr.strip()[:300]}")
    return proc.stdout or ""


def gh_json(*args: str):
    out = gh(*args, check=False)
    try:
        return json.loads(out) if out.strip() else None
    except json.JSONDecodeError:
        return None


# ── failure identity ──────────────────────────────────────────────────────────


def parse_failure_set(log_text: str) -> set[str]:
    """Extract failing test identities from `gh run view --log-failed` output.

    Keys are `<job>|<test>` — the job carries the platform, and failures are routinely
    platform-specific (a Windows-only `cmd` invocation passes on windows and fails on ubuntu),
    so a bare test name would collapse two different facts into one.

    A step that fails without any parseable test (clippy, fmt, a compile error) registers as
    `<job>|step:<step>` so it is still tracked rather than silently dropping to "green".
    """
    failures: set[str] = set()
    steps_with_tests: set[str] = set()
    failing_steps: set[str] = set()

    for line in log_text.splitlines():
        parts = line.split("\t")
        if len(parts) < 3:
            continue
        job, step, content = parts[0].strip(), parts[1].strip(), parts[2]

        m = _TEST_FAILED.search(content)
        if m:
            failures.add(f"{job}|{m.group(1)}")
            steps_with_tests.add(f"{job}|{step}")
            continue
        if _STEP_FAILED.search(content):
            failing_steps.add(f"{job}|{step}")

    # Only fall back to step-level where no individual test was named for that step —
    # otherwise a compile error line inside a test run would double-count.
    for key in failing_steps - steps_with_tests:
        job, step = key.split("|", 1)
        failures.add(f"{job}|step:{step}")
    return failures


def latest_run_for(branch: str, sha: str | None = None) -> dict | None:
    """Most recent completed CI run for a branch, optionally pinned to a commit."""
    rows = gh_json("run", "list", "--branch", branch, "--limit", "20", "--json",
                   "databaseId,headSha,status,conclusion,workflowName") or []
    rows = [r for r in rows if r.get("status") == "completed"]
    if sha:
        exact = [r for r in rows if r.get("headSha", "").startswith(sha[:12])]
        if exact:
            return exact[0]
        return None
    return rows[0] if rows else None


def failed_steps_from_api(run_id: int) -> set[str]:
    """Failed `<job>|step:<step>` keys, taken from GitHub's own job conclusions.

    The log parser infers failure by grepping for `error:`, which silently misses any tool that
    fails without printing that word. `cargo fmt --check` is exactly such a tool — it prints
    `Diff in <path>:<line>:` and exits non-zero — so a PR failing *only* formatting parsed to an
    empty failure set, produced `new=0`, and was promoted toward `ready_for_human` while red.
    Observed on PR #92, where both platforms failed and the shepherd reported no new failures.

    Job conclusions are authoritative and do not depend on a tool's choice of words, so this is the
    floor: whatever the log says, a failed step is a failure.
    """
    if not run_id:
        return set()
    rows = gh_json("run", "view", str(run_id), "--json", "jobs") or {}
    failures: set[str] = set()
    for job in rows.get("jobs", []):
        if job.get("conclusion") != "failure":
            continue
        name = job.get("name", "?")
        steps = [
            st.get("name", "?")
            for st in job.get("steps", [])
            if st.get("conclusion") == "failure"
        ]
        # A job that failed with no failing step still has to register, or it drops to green.
        for step in steps or ["<unknown step>"]:
            failures.add(f"{name}|step:{step}")
    return failures


def run_failure_set(run_id: int) -> set[str]:
    if not run_id:
        return set()
    parsed = parse_failure_set(gh("run", "view", str(run_id), "--log-failed", check=False))
    # Union, not fallback. Named tests keep the differential fine-grained (a *different* test
    # failing at the same count is still a regression); the API floor guarantees a red job can
    # never read as green. Where the parser already named tests for a step, the step key is
    # dropped so one failure is not counted twice.
    steps = failed_steps_from_api(run_id)
    jobs_with_named_tests = {key.split("|", 1)[0] for key in parsed}
    steps = {k for k in steps if k.split("|", 1)[0] not in jobs_with_named_tests}
    return parsed | steps


def baseline_failures(base_sha: str) -> tuple[set[str], str]:
    """Failing set of the base commit, cached per SHA. Returns (failures, provenance)."""
    BASELINE_DIR.mkdir(parents=True, exist_ok=True)
    cache = BASELINE_DIR / f"{base_sha[:12]}.json"
    if cache.exists():
        data = json.loads(cache.read_text(encoding="utf-8"))
        return set(data["failures"]), data.get("provenance", "cache")

    run = latest_run_for(BASE_BRANCH, base_sha)
    provenance = f"exact:{base_sha[:12]}"
    if not run:
        # No run for that exact commit — approximate with the newest completed base run and
        # say so, rather than silently treating the base as green.
        run = latest_run_for(BASE_BRANCH)
        provenance = f"approx:{(run or {}).get('headSha', 'none')[:12]}"

    failures = run_failure_set(run["databaseId"]) if run else set()
    cache.write_text(json.dumps({
        "base_sha": base_sha,
        "failures": sorted(failures),
        "provenance": provenance,
        "computed_at": datetime.now(timezone.utc).isoformat(),
    }, indent=2), encoding="utf-8")
    return failures, provenance


# ── PR state ──────────────────────────────────────────────────────────────────


@dataclass
class Pr:
    number: int
    title: str
    branch: str
    base_sha: str
    labels: list[str] = field(default_factory=list)

    def has(self, label: str) -> bool:
        return label in self.labels

    def count(self, template: str) -> int:
        return sum(1 for n in range(1, 10) if template.format(n) in self.labels)

    @property
    def terminal(self) -> bool:
        return self.has(L_READY) or self.has(L_BLOCKED)


def open_prs() -> list[Pr]:
    rows = gh_json("pr", "list", "--state", "open", "--limit", "50", "--json",
                   "number,title,headRefName,baseRefOid,labels,isDraft") or []
    return [
        Pr(r["number"], r["title"], r["headRefName"], r.get("baseRefOid", ""),
           [l["name"] for l in r.get("labels", [])])
        for r in rows if not r.get("isDraft")
    ]


def add_label(pr: Pr, label: str) -> None:
    gh("label", "create", label, "--force", "--color", "ededed", check=False)
    gh("pr", "edit", str(pr.number), "--add-label", label, check=False)
    pr.labels.append(label)


def remove_label(pr: Pr, label: str) -> None:
    gh("pr", "edit", str(pr.number), "--remove-label", label, check=False)
    pr.labels = [l for l in pr.labels if l != label]


def ci_status(pr: Pr) -> str:
    """`pending` | `success` | `failure` | `none` across all checks for the PR head."""
    rows = gh_json("pr", "checks", str(pr.number), "--json", "state,name") or []
    if not rows:
        return "none"
    states = {str(r.get("state", "")).lower() for r in rows}
    if states & {"pending", "queued", "in_progress", ""}:
        return "pending"
    if states & {"failure", "error", "cancelled", "timed_out"}:
        return "failure"
    return "success"


# ── goals ─────────────────────────────────────────────────────────────────────


# The hat shepherd goals run under: the coding pack with `AskHuman` withheld (policy.toml
# `[[grants]] component = "coding-unattended"`). Authority is decided by the grant, never by the
# caller asserting it, so this names a profile rather than passing a flag.
#
# The `"interactive": False` in the payload below is *not* what does it — that key was sent on every
# goal since this script was written and nothing ever read it. Every shepherd goal therefore held
# AskHuman, parked on an intake question seconds in, and waited for a human who was asleep.
PROFILE = os.environ.get("SHEPHERD_PROFILE", "coding-unattended")


def start_goal(
    description: str, *, mode: str | None = None, max_turns: int = 0
) -> str | None:
    payload = {"project": PROJECT, "interactive": False}
    if mode:
        payload["mode"] = mode
    body = json.dumps({
        "description": description, "domain": "coding", "max_turns": max_turns,
        "profile": PROFILE, "payload": payload,
    }).encode()
    req = urllib.request.Request(f"{DAEMON}/api/goals", data=body,
                                 headers={"Content-Type": "application/json"}, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            data = json.loads(resp.read().decode() or "{}")
            return data.get("id") or data.get("session_id")
    except urllib.error.HTTPError as e:
        log("goal_start_failed", status=e.code, detail=e.read().decode()[:300])
    except Exception as e:
        log("goal_start_failed", detail=str(e)[:300])
    return None


def active_goal_count() -> int:
    """How many sessions are *consuming* capacity right now.

    `parked` is deliberately excluded. A parked session is blocked on a human answer and is
    running nothing, so counting it against a compute cap reserves capacity nobody is using.
    """
    try:
        with urllib.request.urlopen(f"{DAEMON}/api/goals", timeout=15) as resp:
            rows = json.loads(resp.read().decode() or "[]")
    except Exception:
        return 0
    live = {"running", "pending", "starting", "active"}
    return sum(1 for r in rows if str(r.get("status", "")).lower() in live)


def parse_goal_status(payload: dict) -> str | None:
    """Pull a lowercased status from a GET /api/goals/{id} snapshot (or a bare session row)."""
    session = payload.get("session") if isinstance(payload.get("session"), dict) else payload
    if not isinstance(session, dict):
        return None
    status = str(session.get("status", "")).lower().strip()
    return status or None


def goal_status(session_id: str) -> str | None:
    """Session status, `"missing"` on 404, or None when the daemon is unreachable."""
    try:
        with urllib.request.urlopen(f"{DAEMON}/api/goals/{session_id}", timeout=15) as resp:
            data = json.loads(resp.read().decode() or "{}")
        return parse_goal_status(data) or "missing"
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return "missing"
        return None
    except Exception:
        return None


def pending_review_path(pr_number: int) -> Path:
    return PENDING_REVIEW_DIR / f"{pr_number}.json"


def load_pending_review(pr_number: int) -> dict | None:
    path = pending_review_path(pr_number)
    if not path.exists():
        return None
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    return data if isinstance(data, dict) else None


def save_pending_review(pr_number: int, session_id: str, round_n: int) -> None:
    PENDING_REVIEW_DIR.mkdir(parents=True, exist_ok=True)
    pending_review_path(pr_number).write_text(
        json.dumps({"session_id": session_id, "round": round_n}),
        encoding="utf-8",
    )


def clear_pending_review(pr_number: int) -> None:
    path = pending_review_path(pr_number)
    if path.exists():
        path.unlink()


def settle_pending_cold_review(pr: "Pr", *, dry_run: bool) -> str:
    """Resolve a cold-review goal started on a prior tick (F13).

    Labels assert completed reviews. Applying `shepherd:review-N` when a goal *starts* made
    ready_for_human fire after failed sessions. Label only on `succeeded`.

    Returns: `none` | `running` | `labeled` | `failed`.
    """
    pending = load_pending_review(pr.number)
    if not pending:
        return "none"
    sid = pending.get("session_id")
    round_n = pending.get("round")
    if not sid or not isinstance(round_n, int) or round_n < 1:
        log("cold_review_pending_corrupt", pr=pr.number, pending=pending)
        if not dry_run:
            clear_pending_review(pr.number)
        return "none"

    status = goal_status(str(sid))
    if status is None:
        # Fail closed: do not start another review while we cannot see the current one.
        log("cold_review_status_unknown", pr=pr.number, session=sid, round=round_n)
        return "running"
    if status in _LIVE_GOAL_STATUSES:
        log(
            "cold_review_in_flight",
            pr=pr.number,
            session=sid,
            status=status,
            round=round_n,
        )
        return "running"

    if status == "succeeded":
        label = L_REVIEW.format(round_n)
        log(
            "cold_review_succeeded",
            pr=pr.number,
            session=sid,
            round=round_n,
            label=label,
        )
        if not dry_run:
            add_label(pr, label)
            clear_pending_review(pr.number)
        return "labeled"

    log(
        "cold_review_failed",
        pr=pr.number,
        session=sid,
        status=status,
        round=round_n,
    )
    if not dry_run:
        clear_pending_review(pr.number)
    return "failed"


KICKBACK_PROMPT = """\
Pull request #{pr} (branch `{branch}`: {title}) introduced {n} new test failure(s).

NEW failures — these appeared with your change and are yours to fix:
{new_list}

{preexisting_note}
Do this:
  1. `git fetch origin` and check out `{branch}`.
  2. Reproduce a new failure locally before changing anything. A fix you never watched fail
     is a guess.
  3. Fix the cause. Do not delete, skip, or `#[ignore]` a test to get green. If a test is
     genuinely wrong, say so in the commit message and explain why.
  4. Commit and push to `{branch}`.

Stay inside the scope above. Do not refactor, reformat, or fix unrelated things.
"""

COLD_REVIEW_PROMPT = """\
Cold review of pull request #{pr} (branch `{branch}`: {title}). Round {round} of {total}.

You have NO prior context on this change. That is deliberate — review it as written.

  1. `git fetch origin`, check out `{branch}`, read `git diff origin/{base}...HEAD`.
  2. Find real problems: bugs, missing edge cases, security holes, broken invariants.
     Ignore style and formatting — CI already enforces those.
  3. For each suspicion, READ THE ACTUAL CODE at that location and classify it as Real,
     Exaggerated (possible but vanishingly unlikely), or Hallucinated (you misread it).
     Fix only what is Real. Most first-pass findings are not.
  4. If you fix something, add a test that fails without the fix and passes with it. Run it
     both ways — a test you never watched fail proves nothing.
  5. Commit and push to `{branch}`. If you found nothing Real, push nothing and say so.

{preexisting_note}"""


def _preexisting_note(preexisting: set[str]) -> str:
    if not preexisting:
        return ""
    shown = "\n".join(f"  - {f}" for f in sorted(preexisting)[:10])
    more = f"\n  … and {len(preexisting) - 10} more" if len(preexisting) > 10 else ""
    return (
        f"{len(preexisting)} test(s) were ALREADY failing on the base commit before your "
        f"change. They are NOT yours. Do not fix them, do not mention them:\n{shown}{more}\n"
    )


# ── the state machine ─────────────────────────────────────────────────────────


def tick(pr: Pr, *, dry_run: bool) -> None:
    if pr.terminal:
        return

    status = ci_status(pr)
    if status == "pending":
        log("ci_pending", pr=pr.number)
        return
    if status == "none":
        log("ci_missing", pr=pr.number, note="no checks reported; leaving alone")
        return

    # F13: label only after a prior cold-review goal succeeds. If one is still running, wait.
    pending_state = settle_pending_cold_review(pr, dry_run=dry_run)
    if pending_state == "running":
        log("deferred", pr=pr.number, reason="cold review in flight")
        return

    kickbacks = pr.count(L_KICKBACK)
    reviews = pr.count(L_REVIEW)

    pr_run = latest_run_for(pr.branch)
    pr_failures = run_failure_set(pr_run["databaseId"]) if pr_run else set()
    base, provenance = baseline_failures(pr.base_sha) if pr.base_sha else (set(), "no-base")
    new = pr_failures - base
    preexisting = pr_failures & base
    fixed = base - pr_failures

    log("ci_delta", pr=pr.number, new=len(new), preexisting=len(preexisting),
        fixed=len(fixed), base=provenance,
        new_names=sorted(new)[:5] if new else [])

    if new:
        # Believe a new failure only after a re-run — flakes are common enough that spending a
        # kickback on one is the easiest way to waste a night.
        if not pr.has(L_RERUN):
            log("ci_rerun_for_flake", pr=pr.number, candidates=len(new))
            if not dry_run and pr_run:
                gh("run", "rerun", str(pr_run["databaseId"]), "--failed", check=False)
                add_label(pr, L_RERUN)
            return

        if kickbacks >= MAX_KICKBACKS:
            log("blocked", pr=pr.number, reason="kickbacks exhausted",
                new_names=sorted(new)[:10])
            if not dry_run:
                add_label(pr, L_BLOCKED)
            return

        if active_goal_count() >= MAX_CONCURRENT:
            log("deferred", pr=pr.number, reason="at concurrency cap")
            return

        log("kickback", pr=pr.number, attempt=kickbacks + 1, new_names=sorted(new))
        if not dry_run:
            sid = start_goal(KICKBACK_PROMPT.format(
                pr=pr.number, branch=pr.branch, title=pr.title, n=len(new),
                new_list="\n".join(f"  - {f}" for f in sorted(new)),
                preexisting_note=_preexisting_note(preexisting)))
            if sid:
                add_label(pr, L_KICKBACK.format(kickbacks + 1))
                remove_label(pr, L_RERUN)  # next failure is a different failure
                log("kickback_started", pr=pr.number, session=sid)
        return

    # No new failures. Pre-existing ones do not block — that is the whole point.
    if reviews >= COLD_REVIEWS:
        log("ready_for_human", pr=pr.number, reviews=reviews,
            preexisting_ignored=len(preexisting), fixed=len(fixed))
        if not dry_run:
            add_label(pr, L_READY)
        return

    if active_goal_count() >= MAX_CONCURRENT:
        log("deferred", pr=pr.number, reason="at concurrency cap")
        return

    round_n = reviews + 1
    log("cold_review", pr=pr.number, round=round_n, max_turns=COLD_REVIEW_MAX_TURNS)
    if not dry_run:
        sid = start_goal(
            COLD_REVIEW_PROMPT.format(
                pr=pr.number, branch=pr.branch, title=pr.title, base=BASE_BRANCH,
                round=round_n, total=COLD_REVIEWS,
                preexisting_note=_preexisting_note(preexisting),
            ),
            max_turns=COLD_REVIEW_MAX_TURNS,
        )
        if sid:
            # F13: do not label yet — ready_for_human must require a *succeeded* review.
            save_pending_review(pr.number, sid, round_n)
            log("cold_review_started", pr=pr.number, session=sid, round=round_n)


def seed_backlog(path: Path, *, dry_run: bool) -> None:
    tasks = [t.strip() for t in path.read_text(encoding="utf-8").splitlines()]
    tasks = [t for t in tasks if t and not t.startswith("#")]
    log("seed", count=len(tasks), source=str(path))
    for task in tasks:
        while active_goal_count() >= MAX_CONCURRENT and not dry_run:
            log("seed_waiting", reason="at concurrency cap", task=task[:60])
            time.sleep(POLL_SECONDS)
        log("seed_task", task=task[:80])
        if not dry_run:
            log("seed_started", session=start_goal(task), task=task[:60])


# ── self-test ─────────────────────────────────────────────────────────────────

_FMT_FIXTURE = (
    # Real shape from run 31290446697: a failed job whose output contains no test name and no
    # "error:" token anywhere.
    "test (ubuntu-latest)\tFormat check\t2026-08-09T02:30:29Z Diff in "
    "/home/runner/work/liberado/liberado/crates/coder-core/src/tuning.rs:12:\n"
    "test (ubuntu-latest)\tFormat check\t2026-08-09T02:30:30Z Diff in "
    "/home/runner/work/liberado/liberado/crates/coder-tools/src/lib.rs:203:\n"
)

_FIXTURE = (
    "test (ubuntu-latest)\tTests (includes layer-rules gate)\t2026-08-08T07:59:39Z "
    "test tests::background_job_roundtrip_running_then_completed ... FAILED\n"
    "test (ubuntu-latest)\tTests (includes layer-rules gate)\t2026-08-08T07:59:39Z "
    "test tests::validate_with_configured_command_reports_configured_true ... FAILED\n"
    "test (ubuntu-latest)\tTests (includes layer-rules gate)\t2026-08-08T07:59:39Z "
    "test result: FAILED. 127 passed; 2 failed; 0 ignored\n"
    "test (windows-latest)\tTests (includes layer-rules gate)\t2026-08-08T08:03:23Z "
    "test checkpoint::tests::snapshot_then_restore_is_byte_identical ... FAILED\n"
    "clippy\tLint\t2026-08-08T08:03:23Z error: could not compile `liberado-coder-tools`\n"
)


def self_test() -> int:
    got = parse_failure_set(_FIXTURE)
    expected = {
        "test (ubuntu-latest)|tests::background_job_roundtrip_running_then_completed",
        "test (ubuntu-latest)|tests::validate_with_configured_command_reports_configured_true",
        "test (windows-latest)|checkpoint::tests::snapshot_then_restore_is_byte_identical",
        "clippy|step:Lint",
    }
    ok = got == expected
    for label, s in (("missing", expected - got), ("unexpected", got - expected)):
        for item in sorted(s):
            print(f"  {label}: {item}")

    # The identity check the whole design rests on: same count, different tests, still a
    # regression. A numeric comparison waves this through.
    base = {"job|a", "job|b"}
    head = {"job|a", "job|c"}
    regression = bool(head - base)
    print(f"parser: {'ok' if ok else 'FAILED'}")

    # A formatting failure names no test and never prints the word "error", so the log parser
    # cannot see it. This is not hypothetical: PR #92 failed on both platforms and the shepherd
    # reported new=0, because `cargo fmt --check` says `Diff in <path>:<line>:` and exits 1.
    # The parser is *allowed* to return nothing here — the API floor is what must not.
    fmt_only = parse_failure_set(_FMT_FIXTURE)
    fmt_ok = fmt_only == set()
    print(
        f"fmt-invisible-to-parser: {'ok' if fmt_ok else 'FAILED'} "
        f"(parser sees {sorted(fmt_only) or 'nothing'}; the API floor must cover it)"
    )
    ok = ok and fmt_ok
    print(f"identity-not-count: {'ok' if regression else 'FAILED'} "
          f"(equal counts, new={sorted(head - base)})")

    # Platform separation: same test name, different job, must not collapse.
    cross = parse_failure_set(
        "test (ubuntu-latest)\tT\t1Z test foo::bar ... FAILED\n"
        "test (windows-latest)\tT\t1Z test foo::bar ... FAILED\n")
    platform_ok = len(cross) == 2
    print(f"platform-separation: {'ok' if platform_ok else 'FAILED'} ({len(cross)} keys)")

    # F13: snapshot status parsing (label on success needs the right terminal string).
    snap_ok = (
        parse_goal_status({"session": {"status": "Succeeded"}}) == "succeeded"
        and parse_goal_status({"session": {"status": "budget_exhausted"}})
        == "budget_exhausted"
        and parse_goal_status({"status": "running"}) == "running"
        and parse_goal_status({}) is None
    )
    print(f"goal-status-parse: {'ok' if snap_ok else 'FAILED'}")

    # F13: review labels are only asserted after success — start path must not call add_label.
    import inspect
    src = inspect.getsource(tick)
    # The cold-review start arm saves pending state; labeling happens in settle_pending_cold_review.
    label_on_start = "add_label(pr, L_REVIEW" in src
    f13_ok = (not label_on_start) and ("save_pending_review" in src) and (
        "settle_pending_cold_review" in src
    )
    print(
        f"review-label-on-success: {'ok' if f13_ok else 'FAILED'} "
        f"(label_on_start={label_on_start})"
    )

    return 0 if (ok and regression and platform_ok and snap_ok and f13_ok) else 1


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--once", action="store_true")
    ap.add_argument("--watch", action="store_true")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--seed", type=Path)
    ap.add_argument(
        "--reset-baselines", action="store_true",
        help="drop cached base failure sets; use after changing what CI runs, since a "
             "baseline computed from a truncated (fail-fast) run understates the real set "
             "and makes later runs look like regressions",
    )
    args = ap.parse_args()

    if args.reset_baselines:
        removed = sorted(BASELINE_DIR.glob("*.json"))
        for path in removed:
            path.unlink()
        log("baselines_reset", count=len(removed))

    if args.self_test:
        return self_test()
    if not (args.once or args.watch or args.seed):
        ap.error("pick one of --once, --watch, --seed, or --self-test")

    if args.seed:
        seed_backlog(args.seed, dry_run=args.dry_run)

    while True:
        prs = open_prs()
        for pr in prs:
            try:
                tick(pr, dry_run=args.dry_run)
            except Exception as e:
                log("tick_error", pr=pr.number, detail=str(e)[:300])

        pending = [p for p in open_prs() if not p.terminal]
        if args.once or not args.watch:
            log("pass_complete", open_prs=len(prs), still_working=len(pending))
            return 0
        if not pending:
            log("all_terminal", count=len(prs))
            return 0
        time.sleep(POLL_SECONDS)


if __name__ == "__main__":
    sys.exit(main())
