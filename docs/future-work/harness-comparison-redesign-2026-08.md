---
kind: plan
status: draft
authority: advisory
domain: coding-harness
canonical_for: harness-comparison-redesign
open_items: true
---

# Harness comparison redesign — dispatch, wake, and fairness

**Status**: Plan, recorded 2026-08-16. Reviewed against the code at `28dabad` and the job spool
history under `.liberado/harness-jobs/`. No code yet. When this ships, update
[`spec/reference/harness-comparisons.md`](../spec/reference/harness-comparisons.md) in the same PR —
that document describes the daemon topology this plan removes.

**Owner's framing:** *"I want an agent to dispatch an identical prompt to Pi and the Liberado
coding pack, be free to talk to me and work on other things (non-blocking dispatch), and have it
wake the agent when finished. If a run stalls or fails, it wakes the agent to investigate. The
dispatch system must not be a confounding variable. Willing to rip up existing architecture."*

---

## The one-line version

The comparison *substance* (pins, isolation, verifier, journal, archive) is good and the hand-rolled
scripts keep violating it. The comparison *topology* (installed user-context daemon) is wrong and
the spool history shows it failing on infrastructure, not on harnesses. Keep the substance. Replace
the daemon with a per-job detached executor. Do not build an MCP server.

---

## Evidence — both current paths fail, differently

### The durable path fails operationally

Fourteen jobs sit in `.liberado/harness-jobs/`. Every recent terminal state is `Failed`, and the
causes are infrastructure, not harness behavior:

| Job (tail) | Failure | Cause class |
|---|---|---|
| `…9x4nb301` | `required program is not available: pi.cmd` | The windowless login worker has a different PATH than any interactive shell. |
| `…4emvjwaj` | `baseline warm-up failed with exit code: 0xc000013a` | `STATUS_CONTROL_C_EXIT` — a console signal reached the background process. |
| `…4dd3gf6x` | `experiment id does not match the immutable job pins` | A contract bug inside the spool machinery itself. |
| worker log | `harness worker is already running as process 7880` … `7884` | The startup-key worker and manual workers fight over the instance lock. |

The worker was also bypassed for the runs that matter: compares 3 and 4 ran from hand-written
scripts in sibling directories (`life-os-harness-compare3/`, `-compare4/`). One reason is structural:
`engine.rs` refuses any harness set except exactly `["liberado", "pi"]` in that order, and compare 3
needed Liberado + pi + deepagents.

### The hand-rolled path fails methodologically

Compare 3's scripts (`run-liberado.ps1`, `run-pi.ps1`) violate three rules the durable system exists
to enforce:

- Both harnesses shared the **main checkout's** `target/` directory
  (`CARGO_TARGET_DIR=…\life-os\target`). The spec forbids this: Cargo can reuse freshness state or a
  same-named binary from the wrong checkout.
- Liberado ran whatever `target/debug/liberado-coder-run.exe` was last built — not necessarily the
  pinned base commit. This is the exact stale-binary accident `ensure_liberado_runner` was written
  to prevent.
- Worktrees were reused from compare 2 and reset by hand.
- Pi received the prompt flattened to one line; Liberado received it raw. The model-visible input
  differed.

Compare 4's pins record `Temperature 0.1`; the durable system records `sampling=temperature omitted
by both clients`. The two paths cannot currently produce comparable data even when they intend to.

### Conclusion

The operator bypassed the durable system because it is rigid and fragile; the bypass then leaked
methodology. Any redesign that only adds features to the daemon keeps both failure modes. The
fix must make the *careful* path the *easy* path.

---

## Decision 1 — drop the credential boundary

The daemon's stated justification is a privilege boundary: the submitter never holds the provider
key; the user-context worker resolves it from `HKCU\Environment`.

The boundary does not hold at this threat model. The harness child process receives the key, and the
child executes the submitter's own task text — a submitted task can instruct the harness to print
its environment. So either the submitter is trusted (the boundary is unnecessary) or it is not (the
boundary is insufficient). **Owner decision (2026-08-16): the dispatching agent is trusted.** The
executor resolves the credential alias from its own inherited environment. The job directory still
never stores the secret; the alias indirection stays.

This one decision removes: `worker install`, the Windows startup key, the windowless background
binary variant, the content-addressed `.liberado/bin/` store, the HKCU registry read, the host log,
and the scan loop — roughly half of `worker.rs` plus its install tests, and every failure row in the
table above except the experiment-id bug.

## Decision 2 — per-job detached executor, no daemon

`compare submit` writes the job directory exactly as today, then spawns **one detached executor
process for that job** (`liberado-harness-worker run-job <id>`) and returns the job id immediately.
Non-blocking is a property of process spawning, not of a service.

- The executor inherits the submitter's environment (PATH, credential). The `pi.cmd`-not-found and
  Ctrl+C classes die here.
- A spool-wide runner lock (the existing pid-liveness pattern in `journal.rs`/`worker.rs`)
  serializes paid execution per repository. Parallel comparisons would contaminate wall-clock data
  anyway; one at a time is a measurement policy, not a limitation.
- A dead executor leaves a dead lease. The next `status`/`await` read marks the job
  `Failed(host_infrastructure)`. The liveness code already exists. **No auto-resume**: a reboot or
  crash invalidates a benchmark run; resuming would fabricate wall-clock data.
- `compare worker install|start` and the scan-loop mode are deleted. `--once`-style foreground
  execution stays as `compare work <id>` for console diagnosis.

## Decision 3 — no MCP server

MCP was considered and rejected. The wake problem lives in the *calling* agent's harness, not in the
comparison system. MCP progress notifications exist, but client support for long-running
non-blocking tool calls is uneven, and in most clients the call still blocks the turn. An MCP server
is also a long-lived process plus a protocol layer plus a config surface — the same daemon shape
that is already hurting, in a repo that has shipped ten "parses but is never read" config values.

If the Liberado daemon should later dispatch comparisons conversationally, the spine exists:
`EventSource` (`crates/common/src/event.rs`) fans sources into one channel; a
`CompareJobEventSource` watching the spool for terminal states is ~100 lines when actually wanted.
That is a surface over `submit/status/await`, not the architecture.

## Decision 4 — the wake contract is `await`, made stall-aware

`compare await <id>` is already the right primitive: one blocking local process, filesystem events
as the wake hook, a 30-second recovery poll, no model turns. Keep it. Add:

- `--stall-secs <n>`: while the job is non-terminal, if neither `events.jsonl` nor the active
  harness's stdout log has grown in *n* seconds, exit with a distinct error. This is the "wake me if
  it stalls" requirement; it needs file mtimes, not a watchdog daemon.
- Documented exit contract: `0` = `Succeeded`; non-zero = `Failed`, `Cancelled`, await timeout, or
  stall — with the failure class on stderr. The calling agent's harness maps process exit to a
  notification. For OpenCode-style agents: run `await` as a background process. For a shell: run it
  in a terminal. For clients that cannot background processes: `compare status` is a cheap file
  read between other work.

---

## Fairness — the dispatch system must not be a confound

Fixes required before the next baseline, independent of the topology work. Each is small.

| # | Variable | Current state | Fix |
|---|---|---|---|
| F1 | **Turn budget** | `pins.txt` records `pi_turn_cap=client default` — an acknowledged uncontrolled variable. Liberado is capped; pi is not. | Pass pi an explicit cap if its CLI accepts one; otherwise record pi's actual default in `pins.txt` and `report.json`. A budget that differs by construction must be visible in every scoreboard. |
| F2 | **Tool surface** | The coordinator writes Liberado config with `offered_tools = [read_file, write_file, edit_file, run_command]` and `deny = ["git"]`. Native Liberado offers ~21 tools including `git_*` (`coder-runner/main.rs:725` records a live compare that accidentally offered 21 when this config was dropped). Both configurations have produced "Liberado" numbers. | The baseline runs **native system prompt and tool schemas per harness** (roadmap item 5; compare 4's pins already say this). A narrowed toolset is a legitimate *declared experiment variable* — recorded in `experiment.json`, never the silent default. |
| F3 | **Run order** | Always Liberado first. Over multi-hour runs this is a systematic bias (machine-state drift, thermal, background load). | Alternate order per job (or randomize); record the order in `report.json`. Prewarm already equalizes compile caches. |
| F4 | **Metrics roll-up** | `report.json` carries exit codes and commit hashes. Wall clock, turns, and tokens live in artifacts and are re-dug by hand per run — where analysis errors breed. | Roll into `report.json` per harness: `started_at`/`finished_at`/duration (from `run-status.txt`), verifier exit, turns used, tokens in/out (parse Liberado traces and pi `session.jsonl`). Correctness (`accepted = exit 0 ∧ verifier 0`) is already there. |
| F5 | **Sampling** | Durable system omits temperature; compare 4 pinned 0.1 by hand. | One explicit `sampling` pin in the job spec, applied identically to both clients or recorded as omitted. |
| F6 | **Native write-scope policy** | Liberado's own scope gate can fail a task pi ignores (job `…4acx15f` failed on `docs/` paths). Intended — native policies remain — but it makes task selection part of the experiment design. | Document in the operator guide: baseline tasks must stay inside Liberado's granted scope, or the comparison measures policy, not capability. |

"CI passing" as a metric is a later addition: push the archive ref and watch checks. The archive
refs already exist; do not block the redesign on it.

---

## Keep / delete / add

**Keep (the substance):** `contract.rs` (pins, experiment id, failure classes), `journal.rs`
(append-only events, atomic state, leases, cancel file), `preflight.rs` minus the HKCU read, all of
`legacy.rs`'s prepare/run/verify/save mechanics (pinned worktrees, sibling-copy, isolated target
dirs, prewarm, acceptance overlay, common verifier, archive refs), Windows Job Object containment,
`doctor`.

**Delete (~1,000 lines):** `worker install`, startup key, background binary variant,
content-addressed install, HKCU fallback, scan loop, host log. The engine→legacy argv hop
(`legacy_run_args` — a struct serialized into flags and re-parsed). The hardcoded
`["liberado", "pi"]` engine check. Either make `adapter.rs` real or delete it — a trait the engine
bypasses is fake generality, and a third harness is already needed.

**Add (small):**

1. `run-job <id>` executor mode + spawn-on-submit (`--no-spawn` escape hatch).
2. `await --stall-secs`.
3. Dead-lease sweep on `status`/`await` reads.
4. Harness list from the job spec (the contract already carries it); a real `deepagents` adapter
   behind the existing narrow trait boundary — preflight + launch + artifact location; policy stays
   in the coordinator.
5. The `report.json` metrics roll-up (F4).
6. Engine calls typed functions directly; the legacy direct verbs (`prepare`/`run`/`save`) become
   thin aliases over the same path or are removed. One execution path is how the next compare 3
   does not happen.

## Migration — three independently useful PRs

1. **PR 1 — fairness fixes (F1–F5).** Small, no topology change. Delivers clean data even before
   the daemon is gone. Include the `offered_tools` baseline decision explicitly in
   `spec/reference/harness-comparisons.md`.
2. **PR 2 — executor topology.** Spawn-on-submit, `run-job`, stall-aware await, dead-lease sweep,
   deletion of the daemon machinery. Update `harness-comparisons.md` and `AGENTS.md` in the same PR.
3. **PR 3 — engine generalization.** Typed calls, harness list from spec, deepagents adapter,
   single execution path.

Stop after any PR; each leaves the system coherent.

## How we will know it works

- Per AGENTS.md mutation discipline: for each new guard (stall detection, dead-lease sweep, runner
  lock), break the mechanism, watch the test fail, restore it.
- End-to-end dogfood: dispatch one full comparison through the new path from an agent session
  (non-blocking submit, background await, wake on terminal state), and diff its `report.json`
  against a hand-run control on the same task and commit. The two must agree on exit codes and
  verifier results; wall clock within machine noise.
- The acceptance test for ergonomics: the next comparison write-up lives in the repo's job spool,
  not in a sibling directory of PowerShell scripts.

## Open questions

- Does pi's CLI accept an explicit turn/step cap? (Determines F1's shape.)
- Should `submit` refuse to start when another job holds the runner lock, or queue? Refusing is
  simpler and matches the measurement policy; queuing is one spool scan away if it ever matters.
- Does the deepagents adapter belong in this repo or in the pinned fork with a thin launch shim
  here? Leaning shim: comparison policy must not grow a third harness's opinions.

## Non-goals

- An MCP server (Decision 3).
- Auto-resume after host failure (fabricates wall-clock data).
- Parallel paid runs on one machine (contaminates wall-clock data).
- A scheduler, queue priorities, or multi-machine fan-out. One repo, one machine, one run at a
  time is the measurement policy.
