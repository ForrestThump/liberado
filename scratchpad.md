# Agentic mesh — next steps scratchpad

**North star:** goal sessions on Liberado (`Provider` + `Executor` + `ToolRuntime` + domain packs), not VTCode.  
**Architecture:** `docs/architecture/agentic-loops.md`, `docs/architecture/verifiers.md`  
**Roadmap:** `docs/roadmap/rust-native-agentic-coder-plan.md`

Hygiene is not a gate before these — keep coupling rules as you go; extract kernel types only when a second domain would otherwise copy `coder-*`.

---

## Ordered steps

| ID | Step | Why | Status |
|---|---|---|---|
| **A** | **PR-dispatch: liberado-loop sole path + freeze-before-queue** | Connector + criteria: ship real work; worker cannot redefine gates | **done** (2026-07-10) |
| **B** | Production verifier profiles on factory tasks | Config-owned CI-in-the-loop on real runs | **done** (2026-07-10) |
| **C** | Planner + failure-signature repair | Outer loop depth (perceive → plan → act; smart repair) | **done** (2026-07-10) |
| **D** | Eval curriculum green on mock; hybrid live intake; sparse live worker | Empiricism ladder before more live spend | **done** (2026-07-10) |
| **E** | Tuner → draft PR from eval deltas | Meta-loop seed (propose only; humans dispose) | **done** (2026-07-10) |
| **F** | Session SSE for TUI **or** second-domain vault goal | Surfaces / kernel proof (pigeonhole detector) | **done** (2026-07-10) |

### A detail — done

1. Liberado-loop is the **only** coding backend (`CODING_BACKEND=vtcode` rejected at startup).
2. `goal_contract` column + freeze at `submit_pr_factory_task` (policy auto or caller JSON).
3. Worker stamps frozen verifiers onto `liberado-coder-run` request; always includes `git_nonempty_diff` on auto-freeze.
4. Optional submit args: `goal_contract`, `success_criteria`, `verifiers`, `verify_profile`.

### B detail — done

1. `dispatch.yaml` → `verify:` (`default_profile`, `verifiers`, `include_git_nonempty_diff`, `include_validate_cmd`).
2. Env `LIBERADO_VERIFY_PROFILE` overrides default profile; `VALIDATE_CMD` → `factory-validate` command verifier when enabled.
3. Auto-freeze merge order: config verifiers → VALIDATE_CMD → submit verifiers (id wins) → profile expand → git_nonempty_diff.
4. Submitter `verify_profile` / `verifiers` still override production defaults; full `goal_contract` remains authoritative.

### C detail — done

1. Optional **planner** role (`planner.prompt` / `prompt_path`): JSON plan → injected into task context before coder (attempt 0).
2. **Failure-signature repair**: pipeline fails → `FAILURE_CLASS` / `FAILURE_SIGNATURE` / `REPAIR_HINT` + findings.
3. Retry `prior_feedback` uses signature formatting; repair worker goal includes **Repair focus** from latest signature.
4. Modules: `coder-agent/src/planner.rs`, `repair_feedback.rs`.

### D detail — done

1. **Mock curriculum (CI):** `cargo test -p liberado-heuristics-tuner --lib mock_curriculum` — smoke+core (and sample stress) via scripted MockProvider + real Liberado loop.
2. Module: `heuristics-tuner/src/coder_curriculum_mock.rs`; script: `scripts/run-coder-curriculum-mock.ps1`.
3. **Hybrid/live sparse** already landed (`live_scaffold`, worker smoke); documented in `coder-eval-curriculum.md`.
4. Live lessons (intake JSON resilience, contract-path mock worker) stay on lower rungs.

### E detail — done

1. `draft_proposal.rs`: `build_coder_draft_proposal` + `write_coder_draft_proposal` (Decision 14).
2. Coder tuner run writes under `$LIBERADO_DATA_DIR/tuner/<ts>/`:
   - `PROPOSAL.md`, `proposal.json`, `proposed/prompts/coder/coder.md`, `pr_factory_task.json`
3. `recommended` only if accuracy↑, unsafe=0, no scenario regressions, prompt changed.
4. `pr_factory_task.json` is a hand-off for human-approved PR-dispatch submit — not auto-queued.

### F detail — done

1. **`liberado-session` crate:** `GoalSpec`, `SessionEvent`, store, hub, `DomainPackRunner`.
2. **Life pack** (`LifeOpsDemoRunner`): second-domain proof without coder-tools (pigeonhole detector).
3. **Coding pack** (`CodingSessionPack` in coder-agent): Liberado loop behind the same session port.
4. **HTTP/SSE** on `liberado-server`:
   - `GET/POST /api/goals`, `GET /api/goals/{id}`, `GET /api/goals/{id}/stream`, `POST .../cancel`, `GET /api/goals/domains`
5. Surfaces remain clients — wire TUI/WebUI to these endpoints next (no loop ownership).

### Loose-coupling checklist (every PR)

1. Domain packs → mesh only, not each other.
2. Surfaces → contracts/events, not tools/sandbox.
3. PR factory is a **consumer** of `CoderBackend`, not loop owner.
4. Kernel knobs ≠ pack knobs.
5. git/cargo-only → stay in `coder-*`.
6. Second domain would need it → design seam; extract on copy pain.

---

## Notes

- **TUI maturity plan:** `docs/roadmap/tui-maturity-roadmap.md` (audit + T0–T8 vs Claude Code / Grok Build / OpenCode / …). **Coupling:** shared client core for TUI+WebUI (§1.1); no goals/chat logic only in `tui`.
- Mock ladder (always): `cargo test -p liberado-coder-agent --test mock_intake_e2e`
- Mock **curriculum** (always): `cargo test -p liberado-heuristics-tuner --lib mock_curriculum` (or `scripts/run-coder-curriculum-mock.ps1`)
- Live worker (opt-in): `cargo test -p liberado-coder-agent openrouter_deepseek_live_coding_smoke -- --ignored`
- Hybrid intake: `cargo test -p liberado-coder-agent --test live_scaffold -- --ignored`

### Live validation run (2026-07-10)

| Rung | Result | Notes |
|---|---|---|
| Worker smoke | **ok** | trailing-newline assert already softened |
| Intake schema | **ok** (after resilience) | Models return string criteria, missing ids, junk verifier types — hardened in `coder-core` |
| Hybrid intake→mock worker | **ok** | Mock worker now writes paths from frozen contract |
| Intake resilience | landed | `sanitize_draft`, string/map criteria, skip unknown verifiers |

Ready for **D** (eval curriculum) with these live lessons encoded.
