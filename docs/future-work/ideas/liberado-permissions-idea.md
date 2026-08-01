**Excellent.** Let's make progress on **Decision 4** by defining the core abstractions while drawing inspiration from real systems.

### Analysis of Existing Systems (IronClaw + Others)

I reviewed **IronClaw** in detail (along with patterns from PAI and general capability-based systems). Here's what’s relevant:

**IronClaw (Strongest inspiration for us)**:
- Uses **capability-based permissions** inside WASM sandboxes.
- **Zero default / ambient authority** — tools start with nothing.
- Capabilities are **explicitly granted** (e.g., HTTP access, secret access, specific tool invocation).
- **Credential injection at the host boundary** — the LLM/tool never sees raw secrets. The host injects them only for approved actions.
- **Endpoint allowlisting** for network calls.
- Strong emphasis on **containment** and defense-in-depth.
- Capabilities can be narrowly scoped per tool.

This is excellent because it treats permissions as **first-class, attenuatable tokens** rather than vague "allow/deny" lists.

**PAI (Daniel Miessler)**:
- Uses **containment zones** + `PreToolUse` / `PostToolUse` hooks.
- Zones are mostly path-based in the filesystem.
- Enforcement happens via hooks in the agent loop.
- More centralized than IronClaw’s per-tool model.

**Useful patterns to steal**:
- Explicit, named capabilities instead of implicit access.
- Ability to **narrow** (attenuate) capabilities when delegating.
- Enforcement at the **boundary** (inside the MCP/hook or sandbox), not just in the orchestrator.
- Host-mediated access to sensitive resources (especially secrets).

### Recommended Design for Liberado

We’ll adopt a **capability-based model** inspired by IronClaw, adapted to our native Rust MCPs + hybrid data approach.

Core ideas:
- **Zones** = logical scopes of data or systems.
- **Capabilities** = explicit permissions within zones.
- **No ambient authority** — components only have what is explicitly granted.
- **Narrowing on delegation** — liberado (and subagents) can pass *subsets* of capabilities.
- Enforcement lives primarily **inside each MCP/hook**.

This keeps config relatively simple while giving strong containment.

### Proposed Core Abstractions

Here’s a starting point for the types we should define in `crates/common`:

```rust
// crates/common/src/capability.rs

use serde::{Deserialize, Serialize};

/// A logical scope of data or system access.
/// Can represent vault folders or external local systems.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Zone {
    /// Vault-based zones (e.g. tasks/, decisions/, finance-notes/)
    Vault(String),           // e.g. "tasks", "decisions", "reviews"
    
    /// External structured systems
    FinanceActual,           // Actual Budget
    HealthTracking,
    Inventory,
    
    /// Cross-cutting or sensitive groupings
    Sensitive,
    FamilyShared,
}

/// What an actor is allowed to do within a Zone (or globally).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Capability {
    /// Read access within a zone
    Read(Zone),
    
    /// Write access within a zone
    Write(Zone),
    
    /// Read-only summarized/aggregated view (very useful for finance)
    ReadSummary(Zone),
    
    /// Call a specific MCP by name
    ExecuteMcp(String),
    
    /// Future: AppendOnly(Zone), Delete(Zone), etc.
}

/// A set of capabilities that can be narrowed when delegating.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilitySet {
    pub capabilities: Vec<Capability>,
}

impl CapabilitySet {
    /// Returns a new set containing only the capabilities that are also in `other`.
    /// This is how narrowing/attenuation works.
    pub fn intersect(&self, other: &CapabilitySet) -> CapabilitySet {
        CapabilitySet {
            capabilities: self.capabilities
                .iter()
                .filter(|c| other.capabilities.contains(c))
                .cloned()
                .collect(),
        }
    }

    pub fn contains(&self, cap: &Capability) -> bool {
        self.capabilities.contains(cap)
    }
}
```

### Example MCP Wiring Config

We can keep the user-facing config relatively simple:

```yaml
# config/mcps.yaml (or similar)

mcps:
  tasks:
    base_capabilities:
      - Read(Vault("tasks"))
      - Write(Vault("tasks"))
      - Write(Vault("reviews"))
      - ExecuteMcp("tasks")

  finance_summary:
    base_capabilities:
      - ReadSummary(FinanceActual)
      # Note: No Write or full Read — this MCP is intentionally limited

  decisions:
    base_capabilities:
      - Read(Vault("decisions"))
      - Write(Vault("decisions"))
      - Write(Vault("reviews"))
```

### How Narrowing Works at Runtime (Liberado Dispatch)

When liberado dispatches work, it can pass a narrowed `CapabilitySet`:

```rust
// Example dispatch call (conceptual)
liberado.dispatch_subagent(
    goal: "...",
    allowed_capabilities: base_capabilities.intersect(&narrowed_set),
)
```

If a subagent tries to do something outside its granted capabilities, the MCP should reject it cleanly (with a structured error that liberado can understand).

This matches the "dispatcher can narrow, but not expand" model we discussed.

### Key Design Decisions / Trade-offs

| Aspect | Recommendation | Rationale |
|-------|----------------|---------|
| **Enforcement location** | Inside each MCP/hook | Follows IronClaw's "enforce at the boundary" principle. More robust than central hooks. |
| **Narrowing** | Allowed at dispatch time | Gives flexibility while keeping user config simple. |
| **Expansion** | Never allowed | Security invariant. |
| **User config complexity** | Keep relatively simple (base capabilities only) | Avoids the config explosion you were worried about. |
| **Sensitive zones** | Can be marked for stricter handling | E.g., `FinanceActual` or `Sensitive` can have extra logging or limits on widening. |
| **Secrets** | Never passed to models or subagents | Injected at MCP boundary when needed (IronClaw pattern). |

### Next Steps

Would you like me to:

1. **Create a draft file** (`crates/common/src/capability.rs` or similar) with more complete types + documentation?
2. Expand this with **error types** for permission denials and **audit logging** hooks?
3. Define how liberado’s dispatch interface should accept/pass `CapabilitySet`s?
4. Or first discuss any adjustments to the `Zone` or `Capability` enums?

This foundation will influence how we implement the first MCPs and the liberado dispatcher, so getting the types reasonably solid now is valuable.
