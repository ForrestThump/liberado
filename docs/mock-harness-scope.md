# Mock API Harness — Scope

**Goal:** Unlock the remaining IO/network/clock coverage gaps without requiring real API keys or filesystem state.

## What Already Exists

The current test ecosystem is substantial:

| Component | Mock | Where | Notes |
|-----------|------|-------|-------|
| Provider | `MockProvider` | `liberado-provider` (public) | Scripted responses, records requests. **Cannot script errors** — only `MockExhausted`. |
| ToolRuntime | `NoopRuntime`, `InvocationRecordingRuntime` | `liberado-test-support` (public) | Never fail. **Cannot inject IO errors.** |
| RuntimeFactory | `CallRecordingFactory`, `InvocationRecordingFactory` | `liberado-test-support` (public) | Always succeed. **Cannot inject `RuntimeSetupError`.** |
| Provider error | `FailOnceProvider` | `main-agent/tests` (local) | Fails first `complete` with `ProviderError::Transport`. One-off pattern. |
| Tool error | `FailingRuntime` | `orchestrator/tests` (local) | Returns `Err("simulated failure")`. Re-invented per-crate. |
| Tool error | `PoisonableRuntime` | `mcp/tests` (local) | Injects "connection reset by peer", marks transport dead. Closest thing to a real IO mock. |
| Store IO error | `FailOnceContentStore` | `main-agent/tests` (local) | Injects one `StoreError::Io`. |
| Notifier | `MockNotifier` | `executor/src/risk_gated.rs` (local) | Boolean `ok` field. |
| Guidance | `MockGuidance` | `dispatcher/src/lib.rs` (local) | Canned hits. |

The pattern is clear: **error-capable mocks exist, but are locked inside individual test modules.** Consolidating them into a shared harness is the lift.

## What to Build

### Phase 1: Consolidate into `liberado-test-support` (~2-3 hours)

Extract and make public the error-capable patterns, then eliminate local duplicates.

#### 1a. Scriptable `MockProvider` errors

Add `Result<CompletionResponse, ProviderError>` to the script queue so `complete()` can return `Transport`, `RateLimited`, `EmptyResponse`, or `InvalidRequest`.

```rust
// New enum to wrap script entries
enum ScriptEntry {
    Ok(CompletionResponse),
    Err(ProviderError),
}

// MockProvider gains:
impl MockProvider {
    pub fn push_error(&mut self, error: ProviderError);
    pub fn push_response(&mut self, response: CompletionResponse);
}
```

**Unlocks:** ~15 transport/rate-limit/empty-response error paths in dispatcher + executor.

#### 1b. Error-capable `ToolRuntime` mock

Replace the 4+ `MockInner` patterns with one public mock:

```rust
pub struct InvocationRecordingRuntime {
    catalog: Vec<ToolDef>,
    results: HashMap<String, Result<String, String>>,
    invoked: Arc<Mutex<Vec<ToolInvocation>>>,
    default: Result<String, String>,
}

impl InvocationRecordingRuntime {
    pub fn with_error(tool: &str, err: impl Into<String>) -> Self;
    pub fn invoked(&self) -> Vec<ToolInvocation>;
    pub fn catalog(mut self, defs: Vec<ToolDef>) -> Self;
}
```

**Unlocks:** Executor/risk_gated error paths, orchestrator tool failure paths, MCP scoped-runtime error delegation.

#### 1c. `RuntimeFactory` that returns `RuntimeSetupError`

```rust
pub struct FailingFactory {
    error: String,
}

impl RuntimeFactory for FailingFactory {
    async fn runtime_for(...) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        Err(RuntimeSetupError::new(self.error.clone()))
    }
}
```

**Unlocks:** Orchestrator pool-creation failure path, executor factory error handling.

#### 1d. Mock `Notifier`

```rust
pub struct MockNotifier {
    pub ok: AtomicBool,
    pub sent: Mutex<Vec<String>>,
}

impl Notifier for MockNotifier {
    async fn notify(...) -> Result<(), NotifyError> { ... }
}
```

Move the existing `MockNotifier` from `executor/src/risk_gated.rs` into `test-support`.

**Unlocks:** Proposal downgrade notification paths in risk_gated.rs.

#### 1e. Make `sample_proposal()` public

Export `Proposal::sample_pending()` from `liberado-common` (gated on test feature or `#[cfg(test)]`-only re-export).

**Unlocks:** Eliminates ~30 duplicated `Proposal::pending(...)` calls across tests.

### Phase 2: Clock control (~1 hour)

#### 2a. `FrozenClock` or time override in `MockProvider`

For the `Instant::now()` gaps (degraded TTL, budget exhaustion, TTFT recording):

```rust
// In liberado-test-support
pub struct FrozenTimer {
    now: Mutex<Instant>,
}

impl FrozenTimer {
    pub fn advance(&self, d: Duration);
    pub fn now(&self) -> Instant;
}
```

Thread through to relevant mock types via a `Clock` trait (or use an `Arc<AtomicU64>` epoch offset).

**Unlocks:** `purge_expired_degraded` boundary, `WallClockLimit::is_exhausted`, MeteredProvider TTFT boundary.

### Phase 3: Mock `Vault` / filesystem abstraction (~2 hours)

#### 3a. Filesystem stub for IO error paths

```rust
pub struct MemVault {
    files: Mutex<HashMap<String, Vec<u8>>>,
    next_write_error: AtomicBool,
    next_read_error: AtomicBool,
}

impl MemVault {
    pub fn fail_next_write(&self);
    pub fn fail_next_read(&self);
}
```

Or, simpler: wrap `tempfile::TempDir`-based operations behind a trait, then inject errors:

```rust
pub trait VaultOps: Send + Sync {
    fn read(&self, path: &str) -> Result<String, io::Error>;
    fn write(&self, path: &str, content: &str) -> Result<(), io::Error>;
    fn create_dir_all(&self, path: &str) -> Result<(), io::Error>;
}

pub struct RealVault { root: PathBuf }
pub struct FaultyVault {
    inner: RealVault,
    next_write_error: AtomicBool,
    next_read_error: AtomicBool,
}
```

**Unlocks:** Proposal write failure, proposal directory creation failure, grant overlay read/write failure, durable store IO error rehydration.

## What This Enables — Tests By Gap

| Test | Needs | Phase |
|------|-------|-------|
| Provider transport failure during dispatch classification | `MockProvider` scriptable errors | 1a |
| Provider rate limiting triggers backoff | `MockProvider` scriptable errors | 1a |
| Empty provider response → retry / degrade | `MockProvider` scriptable errors | 1a |
| Invalid request (bad schema) → JSON fallback | `MockProvider` scriptable errors | 1a |
| Tool invocation fails with permission error | `InvocationRecordingRuntime` with error | 1b |
| `RiskGatedToolRuntime::invoke` with tool failure | `InvocationRecordingRuntime` with error | 1b |
| `RuntimeFactory::runtime_for` returns error | `FailingFactory` | 1c |
| Proposal downgrade with notifier failure | `MockNotifier` | 1d |
| Purge expired degraded at exact TTL boundary | `FrozenClock` | 2a |
| Wall clock budget exhausted mid-run | `FrozenClock` + `MockProvider` | 2a |
| TTFT recorded correctly for first token | `FrozenClock` + `MockProvider` | 2a |
| Proposal directory creation fails (IO) | `FaultyVault` / failing filesystem | 3a |
| Proposal write fails (IO) | `FaultyVault` / failing filesystem | 3a |
| Grant overlay read fails → defaults | `FaultyVault` / failing filesystem | 3a |
| Durable store rehydration with truncated log | `FaultyVault` / failing filesystem | 3a |

## Effort Estimate

| Phase | Work | Tests Unlocked |
|-------|------|:--------------:|
| 1a: Scriptable provider errors | 30 min (add enum, push_error, update complete) | ~15 |
| 1b: Error-capable ToolRuntime | 30 min (consolidate existing MockInner patterns) | ~8 |
| 1c: FailingFactory | 10 min (new struct) | ~2 |
| 1d: MockNotifier | 10 min (move existing) | ~2 |
| 1e: sample_proposal export | 5 min (one-line change) | — |
| 2a: FrozenClock | 45 min (new struct + thread through) | ~5 |
| 3a: Filesystem stub | 90 min (VaultOps trait + FaultyVault + wire) | ~10 |
| **Total** | **~4 hours** | **~42 tests** |

## What's NOT Worth Building

These would require structural refactoring and are not justified by the coverage value:

- **Log-capture framework** for tracing-only blocks — these have zero behavioral impact. Asserting log content is fragile and low-value.
- **HTTP SSE test server** for server/chat module — would need a full axum test harness. The 0%-coverage modules (chat, telegram, status) are UI/infrastructure glue, not business logic.
- **Real MCP child-process stub** for `live_runtime.rs` — tests already exercise the in-process channel transport. The live runtime is an integration point, not a logic module.

## Where to Put Things

| What | Where |
|------|-------|
| Scriptable `MockProvider` errors | `crates/provider/src/mock.rs` (already public) |
| Error-capable `ToolRuntime` mock | `crates/test-support/src/lib.rs` |
| `FailingFactory` | `crates/test-support/src/lib.rs` |
| `MockNotifier` | `crates/test-support/src/lib.rs` |
| `sample_proposal()` export | `crates/common/src/proposal.rs` |
| `FrozenClock` / `VaultOps` / `FaultyVault` | `crates/test-support/src/lib.rs` |

All additions are to existing crates. No new crate needed.

## Implementation Checklist

### Phase 1: Consolidate error-capable mocks (~2 hours)

- [x] **1a.** Scriptable `MockProvider` errors — add `ScriptEntry` enum, `push_error()`, update `complete()` to return `ProviderError` variants from script queue
- [x] **1b.** Error-capable `InvocationRecordingRuntime` in `test-support` — consolidate existing `MockInner`/`MockToolRuntime` patterns with configurable per-tool errors
- [x] **1c.** `FailingFactory` in `test-support` — `RuntimeFactory` that returns `RuntimeSetupError`
- [x] **1d.** `MockNotifier` in `test-support` — move existing from `executor/src/risk_gated.rs`
- [x] **1e.** Export `sample_proposal()` from `liberado-common` — make existing test helper public

### Phase 2: Clock control (~1 hour)

- [ ] **2a.** `FrozenClock` — `Instant` override for exact-time boundary tests (degraded TTL, budget exhaustion, TTFT)
  - **Partially done:** `clock` module added to `liberado-common` with `test_freeze_at`/`test_thaw`/`test_advance`.
  - `common/catalog.rs` call sites updated to use `crate::clock::now()`.
  - **Blocker:** `provider` does not depend on `liberado-common`, so TTFT recording cannot use the frozen clock without adding the dependency.
  - **Remaining work:** update `executor`'s `run_started` → `clock::now()` and add boundary tests for `WallClockLimit` / degraded TTL.

### Phase 3: Filesystem abstraction (~2 hours)

- [ ] **3a.** `VaultOps` trait + `FaultyVault` — injectable IO errors for proposal write, directory creation, grant overlay read/write, durable store rehydration
