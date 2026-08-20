# Antipattern Analysis — Liberado Repository

**Branch:** `analyze-repository-antipatterns`  
**Date:** 2026-08-19

---

## Summary

The repository has strong architectural discipline (layer rules, mechanical gates, contract documentation) but suffers from implementation-level antipatterns that the architecture cannot prevent.

| Category | Count | Severity |
|----------|-------|----------|
| Config/DRY violations | 3 | High |
| Test reliability | 2 | High |
| Code duplication | 2 | Medium |
| Error handling | 2 | Medium |
| Platform coupling | 2 | Medium |
| Implicit state machines | 1 | Medium |
| Magic constants | 1 | Low |
| Large methods | 1 | Low |
| Trait overhead | 1 | Low |

---

## Detailed Antipatterns

### 1. Config Literal Duplication Across Production Entry Points (HIGH)
**Location:** `crates/coder-runner/src/main.rs`, `crates/acp-bridge/src/coding_run.rs`, `crates/coder-agent/src/session_pack/build.rs`  
**Evidence:** A dedicated scanner test (`config_literal_rules.rs`) exists because 7+ settings have "shipped green while a consumer hardcoded a literal instead of reading them." The `hashline` config divergence between ACP and runner paths caused 15/25 edits to fail.

### 2. God Module Reversion Despite Mechanical Guards (HIGH)
**Location:** `crates/test-support/tests/layer_rules.rs:219-258`  
**Evidence:** A test enforces that `server/src/api.rs` and `config-loader/src/model.rs` must NOT exist as monoliths — they keep "returning." The architecture requires multi-file splits (`api/mod.rs`, `api/chat.rs`, etc.) but developers re-collapse them.

### 3. Process-Global State in Tests Causing Flakiness (HIGH)
**Location:** Multiple test files using `std::env::set_var`/`remove_var`  
**Evidence:** AGENTS.md documents: "`LIBERADO_DATA_DIR` and friends are read by production code; `cargo test` runs a crate's tests concurrently in one binary, so unguarded `set_var` / `remove_var` produces flakes that always pass when re-run alone." A Windows CI failure occurred when a test cleared `GIT_CONFIG_GLOBAL` and deleted the file it named, while a concurrent `git init` in another test crashed.

### 4. Duplicated Offload/Preview Logic (MEDIUM)
**Location:** 
- `crates/executor/src/lib.rs:131-170` (`spill_oversized_result`)
- `crates/coder-sandbox/src/lib.rs:556-574` (`preview_or_offload`)
**Evidence:** Nearly identical logic for truncating oversized tool results, writing to offload directory, and returning head+tail preview — maintained in two places.

### 5. Error Classification by String Matching (MEDIUM)
**Location:** `crates/coder-agent/src/lib.rs:410-425` (`is_retryable`)  
**Evidence:** 
```rust
CoderError::Validation(msg) => !msg.contains(&format!(
    "FAILURE_CLASS: {}",
    repair_feedback::FailureClass::Infrastructure.as_str()
)),
```
Infrastructure failures (disk full) are detected by substring matching on formatted error messages rather than proper error types.

### 6. Magic Numbers with Complex Rationale in Comments (MEDIUM)
**Location:** `crates/executor/src/lib.rs:179-245`  
**Evidence:** Constants like `DEFAULT_MAX_TURNS=8`, `WRAP_UP_TURNS=3`, `DOOM_LOOP_THRESHOLD=3`, `ARG_SIMILARITY_THRESHOLD=0.2` have multi-paragraph comments explaining live-tuning calibration against DeepSeek/Gemini behavior. These are not configurable.

### 7. Implicit State Machines Without Explicit States (MEDIUM)
**Location:** `crates/executor/src/lib.rs:317-338` (`LoopGuard`), `crates/coder-agent/src/progress.rs`  
**Evidence:** The 3-step escalation (Nudge → Remove → GiveUp) uses a `strikes: u8` counter instead of an explicit state machine. Two separate guards (doom-loop, short-cycle) must NOT share a counter — a bug where "whichever mechanism detected a problem second silently skipped its own nudge step."

### 8. Platform-Specific Workarounds Scattered in Core Logic (MEDIUM)
**Location:** `crates/coder-sandbox/src/lib.rs:142-143`, `crates/coder-sandbox/src/lib.rs:330-338`  
**Evidence:** Windows extended path prefix (`\\?\C:\...`) stripping logic appears in multiple places (`strip_extended_path_prefix`, `path_for_cli`, `absolute_path`) rather than a single platform abstraction.

### 9. Mutex for Progress Tracking Instead of Channels (MEDIUM)
**Location:** `crates/coder-agent/src/lib.rs:602-608` (`WorkerRuntime`), `crates/coder-agent/src/progress.rs`  
**Evidence:** `Arc<Mutex<ProgressGuard>>` protects progress state across async boundaries. AGENTS.md warns: "clippy's `await_holding_lock` forces the guard to drop before the first `await`, so it covers the *set* and not the clearing."

### 10. Schema-less Opaque Config Sections (MEDIUM)
**Location:** `docs/spec/architecture/contracts.md:116-125`, `crates/config-loader/`  
**Evidence:** Pack config sections (`[tuning.coder]`) ride through as `toml::Value` — "the pack parses + validates it at composition time." No schema validation; typos in config keys are silent no-ops.

### 11. Test Infrastructure Leaking into Production (LOW)
**Location:** `crates/test-support/Cargo.toml:11-13`  
**Evidence:** The `test-support` crate has a `[[bin]]` target (`mvl-conformance`) and is consumed as a dev-dependency, yet contains production-facing conformance oracle logic. AGENTS.md notes: "dev-dependencies are deliberately exempt so live tests can reach for concrete providers/notifiers."

### 12. Hardcoded Shell Commands in Preflight Instead of Shared Scripts (LOW)
**Location:** `crates/coder-sandbox/src/preflight.rs:128-139`  
**Evidence:** `liberado_ship_preflight_steps()` embeds `cargo fmt --check`, `cargo check`, `cargo test --workspace --no-fail-fast` as string literals. Comment says "Do **not** re-execute GitHub Actions YAML here — share the same commands or scripts CI uses" but they're hardcoded.

### 13. Large Methods with Deep Nesting (LOW)
**Location:** `crates/coder-agent/src/lib.rs:150-279` (`run_attempts`), `crates/coder-agent/src/lib.rs:843-984` (`attempt_body`)  
**Evidence:** 130+ line methods with 5+ levels of nesting, multiple early returns, and complex control flow mixing attempt loops, repair feedback, strategist consultation, and critic review.

### 14. Implicit Contracts via String Literals (LOW)
**Location:** Multiple crates using `"git"`, `"cargo"`, `"RUSTSEC-"` as string literals  
**Evidence:** `crates/coder-sandbox/src/lib.rs:341` (`GIT: &str = "git"`), `crates/coder-sandbox/src/preflight.rs:173` (`"RUSTSEC-"` parsing). No shared constants for external tool names or advisory prefixes.

### 15. Trait Explosion for Simple Abstractions (LOW)
**Location:** Multiple crates (`Provider`, `ToolRuntime`, `RuntimeFactory`, `CoderProviderFactory`, `ReportGate`, `TurnObserver`, `EventSource`, `ConfigSource`, `Notifier`, `DomainPackRunner`, `ConversationStore`, `SessionRecordStore`, `CommandRunner`, `Workspace`)  
**Evidence:** 13+ traits defining narrow waists. While architecturally intentional (per contracts.md), the sheer number creates indirection overhead — e.g., `ToolRuntime` has 3 methods but 5+ implementations across crates.

---

## Recommended Fix Order (by Impact)

1. **Centralize config assembly** — Eliminate literal duplication across 3+ entry points
2. **Extract offload logic to shared crate** — Deduplicate `spill_oversized_result` / `preview_or_offload`
3. **Replace string-matching error classification with typed errors** — Add `FailureClass` to `CoderError` variants
4. **Fix test environment isolation** — Replace `set_var`/`remove_var` with argument-passing
5. **Centralize platform path normalization** — Single `strip_extended_path_prefix` location
6. **Make executor tuning constants configurable** — Move magic numbers to `CoderTuning`
7. **Replace `LoopGuard` counter with explicit state machine** — Prevent guard interference
8. **Replace `Arc<Mutex<ProgressGuard>>` with channel-based progress** — Avoid clippy `await_holding_lock` issue
9. **Add schema validation for opaque config sections** — Catch typos at load time
10. **Extract preflight commands to shared script/config** — Align with CI
11. **Decompose large methods in `LiberadoLoopBackend`** — Extract attempt phases
12. **Centralize external tool constants** — `GIT`, `CARGO`, `RUSTSEC_PREFIX`
13. **Move `mvl-conformance` binary out of test-support** — Separate test infra from production oracle