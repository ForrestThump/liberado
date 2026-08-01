# Property-Based Testing Plan

Date: **2026-07-31**. Zero `proptest`/`quickcheck` usage exists in the workspace today. This
document specifies all opportunities discovered during a fourth-pass codebase audit,
organized by tier and sequenced for implementation. Every item maps to a concrete bug class
from [`docs/spec/architecture/failure-modes.md`](../spec/architecture/failure-modes.md).

## Prerequisites

Add `proptest` as a dev-dependency to each crate. Crate-scoped, not workspace-level:

```toml
[dev-dependencies]
proptest = "1"
```

Required crates (add in this order, one per tier):
- **Tier 1:** `liberado-common`, `liberado-vault`
- **Tier 2:** `liberado-session`, `liberado-executor`, `liberado-config-loader`
- **Tier 3:** `liberado-executor`, `liberado-config-loader`, `liberado-chat-search`, `liberado-common`

Property tests go in a `proptest` submodule. Convention (adapt per crate):

```rust
#[cfg(test)]
mod proptest_tests {
    use proptest::prelude::*;
    // ...
}
```

---

## Tier 1 — Security-Critical, One Crate, High Confidence

### 1. `CapabilitySet::narrow` + `Capability::subsumes`

**Crate:** `liberado-common` → `crates/common/src/capability.rs`

**What to test:**

```rust
fn narrow_commutative(a: CapabilitySet, b: CapabilitySet) -> bool {
    narrow(&a, &b) == narrow(&b, &a)
}

fn narrow_idempotent(a: CapabilitySet, b: CapabilitySet) -> bool {
    narrow(&narrow(&a, &b), &b) == narrow(&a, &b)
}

fn narrow_never_widens(a: CapabilitySet, b: CapabilitySet) -> bool {
    let result = narrow(&a, &b);
    result.iter().all(|c| {
        a.iter().any(|aa| aa.subsumes(c)) && b.iter().any(|bb| bb.subsumes(c))
    })
}

fn narrow_associative(a: CapabilitySet, b: CapabilitySet, c: CapabilitySet) -> bool {
    narrow(&narrow(&a, &b), &c) == narrow(&a, &narrow(&b, &c))
}
```

**Input strategy:** generate `CapabilitySet`s of size 0–10 from all 7 `Capability` variants
(`Read`, `Write(Zone)`, `ExecuteMcp(String)`, `ExecuteTool(String)`, `AskHuman`, `Delegate`,
`PerformTask`). Zone strings: 1–20 chars, alphanumeric + `/`.

**Defends against:** Class 1 (test pointed at wrong object), Class 6 (two things should agree).
`narrow` is the single delegation primitive every subagent's authority flows through. A
mutation flipping `mine.subsumes(theirs)` ↔ `theirs.subsumes(mine)` in one branch survives
current hand-crafted tests.

**Why existing tests miss:** 7 hand-picked cases. The pairwise subsumption matrix over
7 variants × arbitrary set sizes is never exercised.

---

### 2. `resolve_zone` ↔ `resolve_declared_zone` agreement

**Crate:** `liberado-common` + `liberado-config-loader`

- `crates/common/src/catalog.rs` — `resolve_zone` (L149)
- `crates/config-loader/src/model/topology.rs` — `resolve_declared_zone` (L765)

**What to test:**

```rust
fn zone_mirrors_agree(mcp: McpConfig, tool_name: String) -> bool {
    let descriptor = descriptor_from_config(&mcp); // common's catalog_from_config adapter
    resolve_zone(&descriptor, &tool_name) == resolve_declared_zone(&mcp, &tool_name)
}
```

**Input strategy:** generate `McpConfig` with random `default_zone: Option<Zone>`, random
`tools: Vec<ToolImpact>` (0–5 tools, each with random `write_class`, `zone`, boolean
`writes_tool`), and random `zone_from_arg: Option<String>`. Tool name: 1–20 chars.

**Defends against:** Class 6 — the canonical "two things that should agree." The config
crate's `catalog_from_config` calls `resolve_declared_zone`; the runtime catalog built by
`descriptor_from_config` calls `resolve_zone`. A drift between these two mirrors means the
dispatcher's pre-flight zone guard and the runtime's `RiskGatedToolRuntime` silently disagree
about whether a write is permitted.

**Why existing tests miss:** each function has its own unit tests with *different* fixtures;
nothing compares them on the same input. A mutation to tool-override-vs-default priority in
one mirror but not the other passes both suites in isolation.

---

### 3. `write_target` + `zone_write_restriction` fail-closed

**Crate:** `liberado-common` → `crates/common/src/catalog.rs`

- `write_target` (L107)
- `zone_write_restriction` (L177)

**What to test:**

```rust
fn write_target_never_collapses_to_notawrite(
    descriptor: McpDescriptor,
    tool: String,
    args: serde_json::Value,
) -> bool {
    if descriptor.write_tools.contains(&tool) {
        let result = write_target(&descriptor, &tool, &args);
        matches!(result, WriteTarget::Zone(_) | WriteTarget::Undeterminable(_))
    } else {
        true // tool not declared as write — NotAWrite is correct
    }
}

fn zone_write_restriction_guard_equivalence(
    mcp: String,
    tool: String,
    descriptor: McpDescriptor,
    zone_write_classes: Vec<(String, WriteClass)>,
) -> bool {
    let zone = resolve_zone(&descriptor, &tool);
    let restriction = zone_write_restriction(&mcp, &tool, &descriptor, &zone_write_classes);
    match (zone, restriction) {
        (None, None) => true,
        (Some(z), Some(r)) => z == r,
        _ => false,
    }
}
```

**Input strategy:** generate `McpDescriptor` with `write_tools: Vec<String>` (0–5),
`zone_from_arg: Option<String>` (path or empty), `default_zone: Option<Zone>`, and
`tool_zones: Vec<(String, Zone)>`. Arguments: arbitrary `serde_json::Value` trees (0–3
levels deep, mixed types, Unicode keys). Zone strings: 1–20 chars.

**Defends against:** Class 2 (guard off by default). This is the function whose
collapse-to-`NotAWrite` was the F1 vulnerability — flipping the `!` on the `write_tools`
membership check makes every write look like a read and disarms the zone guard.

**Why existing tests miss:** six hand-picked argument shapes. Arbitrary JSON argument trees
with repeated separators, dots, and Unicode in path-addressed `zone_from_arg` are never
generated.

---

### 4. `Vault::validate_rel_path` accept/reject partition

**Crate:** `liberado-vault` → `crates/vault/src/lib.rs` — `validate_rel_path` (L102)

**What to test:**

```rust
fn accepts_safe_components(path: String) -> bool {
    let components: Vec<_> = Path::new(&path).components().collect();
    let is_safe = components.iter().all(|c| {
        matches!(c, Component::Normal(_) | Component::CurDir)
    });
    is_safe == validate_rel_path(&path).is_ok()
}

fn rejects_unsafe_components(path: String) -> bool {
    let has_unsafe = Path::new(&path).components().any(|c| {
        matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_))
    });
    if has_unsafe {
        matches!(validate_rel_path(&path), Err(VaultError::PathTraversal))
    } else {
        true
    }
}

fn composition_safe(p: String, q: String) -> bool {
    if validate_rel_path(&p).is_ok() && validate_rel_path(&q).is_ok() {
        validate_rel_path(&Path::new(&p).join(&q).to_string_lossy().as_ref()).is_ok()
    } else {
        true
    }
}
```

**Input strategy:** generate path strings from an alphabet of safe chars (`a-z0-9_-.`),
special chars (`..`, `/`, `\`, `:` for Windows drive), and Unicode (emoji, CJK). Include
raw Windows drive letters (`C:\...`) and UNC (`\\server\share\...`) to exercise the
`Prefix` arm that Linux CI cannot reach.

**Defends against:** Class 5 (write-only memory — the one-way seam). The failure-modes doc
notes that `to_relative` was correct but only wired to watcher paths, while tool-call paths
reached `write` unvalidated. The property "all public entry points reject the same attack
strings" is the mechanical version of that fix.

**Why existing tests miss:** three escape strings and five safe strings. Swapping
`ParentDir` ↔ `CurDir` in the component match, or deleting the `is_absolute` guard,
survives the current test suite for the covered inputs.

---

### 5. `instruction_scope` + `truncate_to_instruction` invariants

**Crate:** `liberado-common` → `crates/common/src/capability.rs`

- `instruction_scope` (L235)
- `truncate_to_instruction` (L267)

**What to test:**

```rust
fn scope_is_valid_prefix(goal: String) -> bool {
    let scope = instruction_scope(&goal);
    goal.starts_with(&scope) && !scope.is_empty() // scope is a prefix, never empty for non-empty input
}

fn scope_idempotent(goal: String) -> bool {
    instruction_scope(&instruction_scope(&goal)) == instruction_scope(&goal)
}

fn scope_context_invariant(goal: String) -> bool {
    let scope = instruction_scope(&goal);
    let with_context = format!("{}\n\nContext: additional information about the task", goal);
    instruction_scope(&with_context) == scope
}

fn scope_never_panics(goal: String) -> bool {
    let _ = instruction_scope(&goal);
    true
}
```

**Input strategy:** arbitrary Unicode strings 0–2000 bytes. Include mid-multibyte cutoffs
at the 600-byte scan limit, multi-line strings with `Context:`/`Note:` markers after
non-ASCII prefixes, and strings consisting entirely of whitespace/emoji/Zalgo text.

**Defends against:** Class 1 and 6. The pre-flight magnitude guard and `RiskGatedToolRuntime`'s
runtime magnitude check both read `instruction_scope` — the doc records that the runtime used
to read the *raw* goal, a guard-vs-guard Class 6 divergence. The emoji boundary-snap logic
(`-=` on a char-boundary loop) is the classic mutation blind spot: `>`→`>=` changes behavior
only for inputs whose cutoff lands mid-character.

**Why existing tests miss:** ~8 snapshot strings. A 3-byte emoji exactly at the 600-byte
cutoff, arbitrary whitespace/CRLF mixes between `Context:` and the scan limit, and
near-marker false-positive text are a fuzzer's natural habitat; a proptest would have caught
the mid-char slice panic on the first run if it were ever reintroduced.

---

## Tier 2 — High Impact, Pure Logic, Slightly More Setup

### 6. `CompletionGate::evaluate` + quorum fail-closed

**Crate:** `liberado-session` → `crates/session/src/completion_gate.rs`

- `evaluate` (L302)
- `Quorum::approvals_required` (L250)

**What to test:**

```rust
// approvals_required(n) = floor(n/2) + 1, monotone non-decreasing
fn quorum_strict_majority(n: u8) -> bool {
    let r = Quorum::approvals_required(n);
    (r as u16 * 2 > n as u16) && (n == 0 || (r as u16 - 1) * 2 <= n as u16)
}

fn quorum_monotone(a: u8, b: u8) -> bool {
    if a <= b { Quorum::approvals_required(a) <= Quorum::approvals_required(b) } else { true }
}

/// For any gate config and reviewer verdicts, a coerced or missing reviewer
/// can never raise the approval count.
fn gate_fail_closed(
    config: GateConfig,
    gatekeeper: Verdict,
    fresh_votes: Vec<(u8, Verdict)>, // (reviewer id, verdict)
) -> bool {
    let result = evaluate(&config, gatekeeper, &fresh_votes);
    if result == GateOutcome::Approved {
        let coerced_or_missing = fresh_votes.iter().any(|(_, v)| matches!(v, Verdict::Err));
        // No coerced/missing vote should have been counted as approval
        !coerced_or_missing && gatekeeper == Verdict::Approve
            && fresh_votes.iter().filter(|(_, v)| v == &Verdict::Approve).count()
                >= Quorum::approvals_required(config.reviewers)
    } else {
        true
    }
}

/// Gatekeeper veto: non-approving gatekeeper → Refuted with exactly 1 vote.
fn gatekeeper_veto(gatekeeper: Verdict, fresh_votes: Vec<(u8, Verdict)>) -> bool {
    let config = GateConfig { reviewers: fresh_votes.len() as u8, ..Default::default() };
    let result = evaluate(&config, gatekeeper, &fresh_votes);
    if gatekeeper != Verdict::Approve {
        matches!(result, GateOutcome::Refuted { vote_count: 1, .. })
    } else {
        true
    }
}
```

**Input strategy:** `n` in 0..=u8::MAX. Gate config: reviewers 0..=5, quorum 0..=reviewers.
Verdicts: `Approve`, `Refute`, `Err` (representing a reviewer that errored out). Fresh votes:
random per-reviewer verdict.

**Defends against:** Class 1 and 4. This module exists to enforce fail-closed against a
sick reviewer. A mutated comparison bound (`>=`→`>` on required approvals) only gets caught
when the enumerated config sits exactly at the boundary.

**Why existing tests miss:** six hand-crafted gate configurations. The entire space of
`3^reviewers × gatekeeper × config` is never walked.

---

### 7. `Proposal` note round-trip + signature survival

**Crate:** `liberado-common` → `crates/common/src/proposal.rs`

- `to_note` (L208)
- `from_note` (L221)
- `ProposalSigner` (L298)

**What to test:**

```rust
fn proposal_note_roundtrip(proposal: Proposal) -> bool {
    from_note(&proposal.to_note()) == Ok(proposal)
}

fn signature_survives_note(proposal: Proposal, signer: ProposalSigner) -> bool {
    let signed = signer.sign(proposal);
    let parsed = from_note(&signed.to_note()).unwrap();
    signer.verify(&parsed)
}

fn human_edit_preserves_status_only(proposal: Proposal, signer: ProposalSigner) -> bool {
    let mut signed = signer.sign(proposal);
    let note = signed.to_note();
    let edited = note.replace("status: pending", "status: approved");
    let parsed = from_note(&edited).unwrap();
    parsed.status == ProposalStatus::Approved && signer.verify(&parsed)
}
```

**Input strategy:** generate proposals with all five `ProposedAction` variants (`ToolCalls`,
`Subagent`, `External`, `VaultWrite`, `Other(Value)`). Rationale: arbitrary strings including
newlines, Unicode, `---` (YAML frontmatter fence), `#` (markdown headings),
and `status:`-like inline keywords. `requested_grant`: `CapabilitySet` 0–3 caps.
`pool`/`approved_scope`/`expires`: randomly `Some` or `None`.

**⚠️ May find a real defect.** A rationale containing a line beginning with `---` may collide
with `extract_frontmatter`'s first-`---` fence scan in the YAML adapter. If the property
fails on this input, that is a latent bug in the on-disk format — the daemon would read a
different proposal than what was written.

**Defends against:** Class 6 — the on-disk note and the struct the daemon parses at approval
time must agree; a divergence means a human "approved" a proposal the daemon reads differently.

**Why existing tests miss:** four fixed round-trip cases (ToolCalls/Subagent/External/
permission-request). `VaultWrite` and `Other(Value)` have zero round-trip tests. None
exercise strings that can collide with the frontmatter fence. No test checks signed-then-
round-tripped verification.

---

### 8. `args_similarity` / `cosine` / `tokenize` — symmetry and range

**Crate:** `liberado-executor` → `crates/executor/src/lib.rs`

- `args_similarity` (L1241)
- `cosine` (L1308)
- `tokenize` (L1270)

**What to test:**

```rust
fn similarity_symmetric(a: serde_json::Value, b: serde_json::Value) -> bool {
    (args_similarity(&a, &b) - args_similarity(&b, &a)).abs() < 1e-5
}

fn similarity_reflexive(x: serde_json::Value) -> bool {
    (args_similarity(&x, &x) - 1.0).abs() < 1e-5
}

fn similarity_in_range(a: serde_json::Value, b: serde_json::Value) -> bool {
    let s = args_similarity(&a, &b);
    s >= 0.0 && s <= 1.0
}

fn identity_args_force_zero(a: serde_json::Value, b: serde_json::Value) -> bool {
    // If both have the same identity key set to different strings, similarity = 0
    // Test: for deliberate identity-key colliders, result is exactly 0.0
    ...
}

fn tokenize_never_panics(s: String) -> bool {
    let _ = tokenize(&s);
    true
}
```

**Input strategy:** generate arbitrary `serde_json::Value` trees (0–4 levels, mixed
numbers/strings/bools/arrays/objects, NaN in numeric args, Unicode keys). Include objects
with identity keys (`IDENTITY_ARG_KEYS` in the config) set to different strings.

**Defends against:** Class 6 — the doom-loop guard's detection threshold rides on this
scoring. A drift between how the pre-flight and runtime judge argument similarity changes
whether runaway loops are caught.

**Why existing tests miss:** three fixed argument pairs plus two cosine tests. Symmetry,
reflexivity, and range over generated JSON trees are never asserted. A mutation breaking
symmetric weighting is invisible.

---

### 9. `mcp_of` / `bare_tool_name` / `grants_tool` agreement

**Crate:** `liberado-common`

- `crates/common/src/capability.rs` — `grants_tool` (L399), `grants_mcp` (L389)
- `crates/common/src/dispatch.rs` — `mcp_of` (L240), `bare_tool_name` (L248)

**What to test:**

```rust
fn tool_name_reconstruction(name: String) -> bool {
    format!("{}:{}", mcp_of(&name), bare_tool_name(&name)) == name
}

fn server_grant_authorizes_all_tools(mcp: String, tool: String) -> bool {
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp(mcp.clone())]);
    let full = format!("{}:{}", mcp, tool);
    caps.grants_tool(&full)
}

fn tool_grant_authorizes_only_specific_tool(mcp: String, tool: String, other: String) -> bool {
    let caps = CapabilitySet::from_iter([Capability::ExecuteTool(format!("{}:{}", mcp, tool))]);
    let specific = format!("{}:{}", mcp, tool);
    let different = format!("{}:{}", mcp, other);
    caps.grants_tool(&specific)
        && (tool == other || !caps.grants_tool(&different))
}
```

**Input strategy:** tool names: 0–30 chars, including no-colon, single-colon, multi-colon
(`"a:b:c"`), leading/trailing colons, empty strings. MCP names: 0–20 chars. Capability
sets: `ExecuteMcp` + `ExecuteTool` mix, 0–5 caps.

**Defends against:** Class 1 and 6 — this is the authorization question both the dispatcher
guard and `RiskGatedToolRuntime` ask. The doc records the historical bug where collapsing
to `grants_mcp` silently passed partial grants.

**Why existing tests miss:** four fixed cases. The "for every tool string" totality is
existential in unit tests — a proptest makes it universal.

---

### 10. Session state machine invariants under operation sequences

**Crate:** `liberado-session`

- `crates/session/src/goal.rs` — `check_session_invariants` (L372)
- `crates/session/src/store.rs` — `GoalSessionStore`

**What to test:**

```rust
fn invariants_hold_after_every_op(
    ops: Vec<StoreOp>,
) -> proptest::test_runner::TestCaseResult {
    let store = GoalSessionStore::new();
    let mut ids = Vec::new();

    for op in &ops {
        match op {
            StoreOp::Insert(record) => {
                store.insert(record.clone());
                ids.push(record.id.clone());
            }
            StoreOp::PushEvent(id, event) => {
                store.push_event(id, event.clone());
            }
            StoreOp::SetStatus(id, status) => {
                store.set_status(id, *status);
            }
            StoreOp::Finish(id, status, result) => {
                store.finish(id, *status, result.clone());
            }
        }
        // Check invariants after every operation
        for id in &ids {
            if let Some(record) = store.get(id) {
                crate::check_session_invariants(&record)?;
            }
        }
    }

    Ok(())
}

fn replay_never_panics_and_produces_invariant_records(
    lines: Vec<LogLine>,
) -> proptest::test_runner::TestCaseResult {
    let dir = tempdir();
    let log = dir.path().join("replay.jsonl");
    let json = lines.iter()
        .map(|l| serde_json::to_string(l).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&log, json).unwrap();

    let store = GoalSessionStore::open(dir.path()).await;
    // Every replayed record must pass invariants
    for (id, record) in store.iter() {
        crate::check_session_invariants(&record)?;
    }

    Ok(())
}
```

**Input strategy:** `StoreOp` enum: `Insert(GoalSessionRecord)`, `SetStatus(SessionStatus)`,
`Finish(SessionStatus, GoalResult)`, `PushEvent(SessionEvent)`. Generate random sequences
of 0–20 ops targeting 1–5 session IDs. `LogLine`: model-strategy pairs for Start, Status,
Event, Finish — including torn/altered sequences to exercise the rehydration coercion path
(Parked → Failed, no-Finish → Failed).

**Defends against:** Class 1 — the failure-modes doc's single most expensive class. The
store's in-memory double and the JSONL replay are two implementations of one lifecycle;
the invariant checker currently only runs once at a single daemon test site.

**Why existing tests miss:** each operation is unit-tested in isolation; the state machine
as a reachability graph is never walked. A mutation that makes `finish` set `awaiting_input`
true, or makes `set_status(Running)` on a finished session legal, survives because no test
ever builds that exact reachable state. This is an async test (tokio spawn internally), but
proptest's blocking strategies fit fine since ops are in-process and fast.

---

## Tier 3 — Valuable, Narrower Surface

### 11. `merge_tables` associativity / idempotence + overlay priority

**Crate:** `liberado-config-loader` → `crates/config-loader/src/chain.rs` (L132)
and `crates/config/src/lib.rs` — `merge_overlay_into` (L317)

**What to test:**

```rust
fn merge_idempotent(a: toml::Value) -> bool {
    merge_tables(&a, &a) == a
}

fn merge_associative(a: toml::Value, b: toml::Value, c: toml::Value) -> bool {
    merge_tables(&merge_tables(&a, &b), &c) == merge_tables(&a, &merge_tables(&b, &c))
}

fn overlay_never_downgrades_write_class(
    base: Policy,
    overlay: Policy,
    zone: Zone,
) -> bool {
    if overlay.write_class(&zone) == WriteClass::AgentWritable {
        let merged = merge_overlay_into(base.clone(), &overlay);
        merged.write_class(&zone) == base.write_class(&zone)
    } else {
        true
    }
}
```

**Input strategy:** generate random `toml::Value` trees (tables, arrays, strings, ints,
floats, bools, datetimes) 0–5 levels deep. For overlay priority: random policies with
0–10 zones, each with random `WriteClass`.

**Defends against:** Class 6 — the merged config is what the daemon enforces; a merge
mutation silently changes the enforcement surface.

**Why existing tests miss:** five merge fixtures, one overlay-priority test. Deeply nested
TOML trees and random zone-set intersections are unexercised.

---

### 12. `mentions_destructive` / `assess_magnitude` language properties

**Crate:** `liberado-common` → `crates/common/src/capability.rs`

- `mentions_destructive` (L174)
- `assess_magnitude` (L195)
- `is_sweeping_destructive` (L205)

**What to test:**

```rust
fn destructive_stems_always_detected(mut stem: String) -> bool {
    // Stems: delet, remov, wipe, purge, eras, destroy, drop, truncat, overwrit
    let known = ["delet", "remov", "wipe", "purge", "eras", "destroy", "drop", "truncat", "overwrit"];
    for k in &known {
        if stem.starts_with(k) {
            return mentions_destructive(&stem);
        }
    }
    true
}

fn sweeping_words_classified(prefix: String, word: String) -> bool {
    let phrase = format!("{} {}", prefix, word);
    is_sweeping_destructive(&phrase)
        == SWEEPING_WORDS.iter().any(|w| word.contains(w))
}

fn case_invariant(text: String) -> bool {
    is_sweeping_destructive(&text.to_uppercase()) == is_sweeping_destructive(&text)
        && assess_magnitude(&text.to_uppercase()) == assess_magnitude(&text)
}

fn never_panics(text: String) -> bool {
    let _ = mentions_destructive(&text);
    let _ = assess_magnitude(&text);
    let _ = is_sweeping_destructive(&text);
    true
}
```

**Input strategy:** strings 0–200 chars, arbitrary Unicode, embedded punctuation, mixed case.
Include words starting with each destructive stem followed by random suffixes, words containing
substrings of sweeping words, and the exact word-boundary edge case that caused the D1
dogfood regression (`"clear"` alone vs `"clear the files"`).

**Defends against:** Class 1 and 4 — these classifiers sit in the gate that *downgrades* work.
Both documented false-positive failures came from word-boundary and case-sensitive corner cases
a generator would flood.

**Why existing tests miss:** ~10 snapshot strings. The tables are `const` arrays; a token
added to `SWEEPING_WORDS` that a `words()` filtering change silently drops is invisible to
fixed inputs. Properties over generated strings make the tables themselves the contract.

---

### 13. `Policy::write_class` first-match + `capabilities_for` union

**Crate:** `liberado-config-loader` → `crates/config-loader/src/model/policy.rs` (L23, L39)

**What to test:**

```rust
fn write_class_first_match_wins(policy: Policy, zone: Zone) -> bool {
    let expected = {
        let mut found = WriteClass::default();
        for z in &policy.zones {
            if z.zone == zone {
                found = z.class;
                break;
            }
        }
        found
    };
    policy.write_class(&zone) == expected
}

fn capabilities_for_union(
    grant: ComponentGrant,
    relevant: Vec<Capability>,
    irrelevant: Vec<Capability>,
) -> bool {
    let caps = capabilities_for(&grant);
    relevant.iter().all(|c| caps.contains(c))
        && irrelevant.iter().all(|c| !caps.contains(c))
}
```

**Input strategy:** random policies with 0–10 zones (each with random `Zone` name, `WriteClass`).
Grants: all 5 `ComponentGrant`/`CapabilityGrantEnum` variants with random capability lists.

**Defends against:** Class 6 — `Policy` is the single authority file; both the dispatcher ceiling
and `RiskGatedToolRuntime` read it.

**Why existing tests miss:** the `write_class` unlisted-zone default is asserted once; the
full "declared zone with a later duplicate wins" behavior and the `capabilities_for` union
for every grant variant are unspecified and untested.

---

### 14. `Budget` exhaustion monotonicity + no-overflow

**Crate:** `liberado-executor` → `crates/executor/src/budget.rs`

- `ResourceLimit::is_exhausted` (L19)
- `exhausted_extra` (L114)

**What to test:**

```rust
fn exhaustion_monotone(
    turns1: u32, elapsed1: Duration, tokens1: u64,
    turns2: u32, elapsed2: Duration, tokens2: u64,
    max_elapsed: Duration, max_tokens: u64,
) -> bool {
    if turns1 <= turns2 && elapsed1 <= elapsed2 && tokens1 <= tokens2 {
        let u1 = Usage { turns: turns1, elapsed: Some(elapsed1), tokens: tokens1, ..Default::default() };
        let u2 = Usage { turns: turns2, elapsed: Some(elapsed2), tokens: tokens2, ..Default::default() };
        let exhausted = |u: &Usage| {
            TurnLimit(turns1).is_exhausted(u)
                || WallClockLimit(max_elapsed).is_exhausted(u)
                || TokenLimit(max_tokens).is_exhausted(u)
        };
        exhausted(&u1) == exhausted(&u2) || (!exhausted(&u1) && exhausted(&u2))
    } else { true }
}

fn wall_clock_exhausted_at_and_after_boundary(
    elapsed: Duration,
    limit: Duration,
) -> bool {
    WallClockLimit(limit).is_exhausted(&Usage {
        elapsed: Some(elapsed), ..Default::default()
    }) == (elapsed >= limit)
}

fn token_limit_no_overflow() -> bool {
    let _ = exhaustive_extra(&Usage::default(), &[TokenLimit(u64::MAX), WallClockLimit(Duration::MAX)]);
    true
}
```

**Input strategy:** turns 0..=u32::MAX, tokens 0..=u64::MAX, elapsed random durations.
Limits at extremes (u32::MAX, u64::MAX, Duration::MAX, Duration::ZERO).

**Defends against:** Class 1 — budget logic is exercised only through run-loop integration
tests; it has **zero unit tests** today (no `#[cfg(test)]` module in `budget.rs`).

**Why existing tests miss:** no test asserts the exhaustion predicates outside the run loop.
A `>=`→`>` off-by-one or a wrapping addition is invisible.

---

### 15. `pct_of_window` / `resolve_trigger_tokens_with_source` no-panic

**Crate:** `liberado-config-loader` → `crates/config-loader/src/model/topology.rs`

- `pct_of_window` (L319)
- `resolve_trigger_tokens_with_source` (L281)

**What to test:**

```rust
fn pct_of_window_never_panics(window: u32, pct: f32) -> bool {
    let result = pct_of_window(window, pct);
    result >= 1 && result <= window.max(1)
}

fn trigger_resolution_priority_ladder(
    config: CompactionConfig,
    model: Option<String>,
) -> bool {
    let result = resolve_trigger_tokens_with_source(&config, model.as_deref());
    // Priority: per-model absolute > per-model pct > global absolute > global pct > fallback
    // Verify result matches a reference walk
    ...
    result <= config.context_window.max(1) as u32
}
```

**Input strategy:** window 0..=u32::MAX, pct including `NaN`, `Infinity`, `-Infinity`,
`-1.0`, `0.0`, `0.5`, `1.0`, `2.0`. Random compaction configs with per-model and global
absolute + pct settings.

**Defends against:** Class 1/6 — compaction trigger is the CH3 compaction gate's guard.
A `NaN` silently saturating to 0 would fire compaction every turn.

**Why existing tests miss:** six fixed configs. No test ever passes a non-finite `f32`. The
`clamp`/`as u32` casting path is a panic site for extreme values that the existing tests
never exercise.

---

### 16. `ParsedQuery::matches` / `find_start` / `snippet` no-panic

**Crate:** `liberado-chat-search` → `crates/chat-search/src/query.rs` + `scan.rs`

**What to test:**

```rust
fn matches_reference_agreement_literal(terms: Vec<String>, haystack: String) -> bool {
    let q = ParsedQuery::literal(&terms.join(" "));
    let expected = terms.iter().all(|t| haystack.to_lowercase().contains(&t.to_lowercase()));
    q.matches(&haystack) == expected
}

fn find_start_consistency(terms: Vec<String>, haystack: String) -> bool {
    let q = ParsedQuery::literal(&terms.join(" "));
    if q.matches(&haystack) {
        let start = q.find_start(&haystack);
        start.is_some() && start.unwrap() <= haystack.len()
    } else {
        true
    }
}

fn snippet_never_panics(content: String, terms: Vec<String>) -> bool {
    let q = ParsedQuery::literal(&terms.join(" "));
    let _ = snippet(&content, &q, 200);
    true
}
```

**Input strategy:** terms: 0–5 strings, 1–20 chars each, including empty strings, Unicode,
regex-like text (`.*`, `[a-z]`). Haystack: 0–2000 chars, arbitrary Unicode. Include
strings where a match offset falls mid-multibyte character (tests `floor_char_boundary`/
`ceil_char_boundary` safety).

**Defends against:** Class 1 — the search MCP's whole history surface. `snippet` has no
direct unit test at all and the offset math only runs when a file matches.

**Why existing tests miss:** six query fixtures. `snippet` is untested. The `floor_char_boundary`
/`ceil_char_boundary` calls with a center that can land mid-char are the exact panic site
for multibyte offsets introduced by adversarial inputs.

---

### 17. `UserTimezone` parse/round-trip + `context_line_at` determinism

**Crate:** `liberado-common` → `crates/common/src/local_time.rs` (L39, L78, L97)

**What to test:**

```rust
fn timezone_roundtrip(name: String) -> bool {
    if let Ok(tz) = UserTimezone::parse(&name) {
        UserTimezone::parse(&tz.iana_name()).unwrap() == tz
    } else {
        true
    }
}

fn empty_or_whitespace_rejected(name: String) -> bool {
    if name.trim().is_empty() || name.chars().all(char::is_whitespace) {
        UserTimezone::parse(&name).is_err()
    } else {
        true
    }
}

fn context_line_at_deterministic(utc: i64) -> bool {
    let t = chrono::DateTime::from_timestamp(utc, 0).unwrap_or_default();
    context_line_at(&t) == context_line_at(&t)
}
```

**Input strategy:** timezone names: arbitrary strings 0–100 chars (`"UTC"`, `"America/Chicago"`,
`"Pacific/Auckland"`, `""`, `"   "`, `"NotARealTz"`, alphabetic garbage, numeric-only).
UTC timestamps: i64::MIN..=i64::MAX (tests extreme date under/overflow paths in chrono).

**Defends against:** Class 6/1 — low criticality but zero coverage. The agent time-
stamping logic is exercised only through full-daemon integration.

**Why existing tests miss:** three fixed names. The empty/whitespace/unknown partition and
determinism of `context_line_at` are never checked.

---

## Implementation Sequence

1. **Tier 1** (`liberado-common` + `liberado-vault`, ~2 hours)
   - Items 1–5. One `proptest` dev-dep, all pure functions, no async.
   - Start here: these defend the two most expensive bug classes (1 and 6).

2. **Tier 2** (`liberado-session` + `liberado-common` + `liberado-executor`, ~3 hours)
   - Items 6–10. Async test for item #10 (tokio + proptest).
   - ⚠️ Item #7 may find a real defect. Run it first and report.

3. **Tier 3** (assorted: `config-loader`, `chat-search`, `common`, ~2 hours)
   - Items 11–17. Lower criticality, narrower surface.
   - Budget (item #14) has zero unit tests today — consider promoting to Tier 2.

**Total estimated effort:** ~7 hours for all 17 properties, ~16 hours for all 45 individual
property assertions including edge-case strategy tuning.
