//! Resource budgets for the agent loop (wall-clock, tokens, turn caps).

use std::sync::Arc;

use crate::DEFAULT_MAX_TURNS;

/// A single bounded resource an execution must respect, checked once per turn against the
/// accumulated [`ResourceUsage`] snapshot. New resource types (a rate limit, anything else
/// bounded) just implement this — `Executor::run_loop`'s own logic never has to change to add
/// one, only a new [`Budget::with_limit`] call site does. Deliberately abstract rather than a
/// hardcoded enum of resource kinds: today's two concrete uses (wall-clock, a token-count proxy
/// for cost — see [`TokenLimit`]'s doc comment for why not real dollars yet) shouldn't be the
/// ceiling on what this can bound later.
pub trait ResourceLimit: Send + Sync {
    /// Human-readable name for diagnostics ("wall-clock", "tokens") — surfaced in a budget-
    /// exceeded failure report so it names *which* resource ran out, not just "turns."
    /// A fixed label for this resource — `"wall-clock"`, `"tokens"`. `'static` because it has
    /// to outlive the borrow of the limit: the name travels out in `ExecError::BudgetExceeded`
    /// so the failed report can say which bound was actually hit.
    fn name(&self) -> &'static str;
    /// Whether this resource has been exhausted given the current usage snapshot.
    fn is_exhausted(&self, usage: &ResourceUsage) -> bool;
}

/// Accumulated resource usage for one execution, updated once per turn. Adding a new
/// [`ResourceLimit`] later may need a new field here — a small, additive change; existing limits
/// and `run_loop`'s own logic don't need to change alongside it.
#[derive(Debug, Clone, Default)]
pub struct ResourceUsage {
    pub turns: u32,
    pub elapsed: std::time::Duration,
    /// Total tokens (prompt + completion) spent so far — see [`TokenLimit`]'s doc comment.
    pub tokens: u64,
}

/// Bounds real elapsed time, independent of turn count — a single slow tool call or a model that
/// just takes a long time per turn isn't caught by a turn cap alone.
pub struct WallClockLimit(pub std::time::Duration);

impl ResourceLimit for WallClockLimit {
    fn name(&self) -> &'static str {
        "wall-clock"
    }
    fn is_exhausted(&self, usage: &ResourceUsage) -> bool {
        usage.elapsed >= self.0
    }
}

/// A stand-in for a real dollar-cost cap: total token count, not actual `$`. Real pricing needs a
/// per-model `$`/token table (rates differ by provider and by prompt vs. completion token, and
/// need upkeep as providers change prices) that doesn't exist yet — deferred until it's clearly
/// worth that upkeep, since current model usage is cheap enough not to need it now. Token count is
/// a reasonable proxy in the meantime: it already correlates with real cost, and it's free (every
/// `CompletionResponse` already reports it) — no new plumbing to add real dollars later either,
/// just a new `ResourceLimit` impl reading a pricing table instead of a raw count.
pub struct TokenLimit(pub u64);

impl ResourceLimit for TokenLimit {
    fn name(&self) -> &'static str {
        "tokens"
    }
    fn is_exhausted(&self, usage: &ResourceUsage) -> bool {
        usage.tokens >= self.0
    }
}

/// Loop bounds: a turn cap (`max_turns`, unchanged from before — still the mechanical driver of
/// `run_loop`'s own iteration, including the doom-loop guard's one-time recovery top-up, which is
/// specifically a turn-count adjustment) plus an open-ended list of additional [`ResourceLimit`]s
/// checked alongside it every turn. `Budget::new`/`Budget::default` build a turns-only budget —
/// unchanged behavior for every existing call site — `.with_limit`/`.with_wall_clock`/
/// `.with_token_limit` opt a call site into additional bounds.
#[derive(Clone)]
pub struct Budget {
    /// Maximum model turns before the loop is force-terminated.
    pub max_turns: u32,
    extra_limits: Arc<Vec<Box<dyn ResourceLimit>>>,
}

impl Budget {
    pub fn new(max_turns: u32) -> Self {
        Self {
            max_turns,
            extra_limits: Arc::new(Vec::new()),
        }
    }

    /// How many extra [`ResourceLimit`]s this budget carries.
    ///
    /// `extra_limits` is private, so a caller adjusting the turn cap has no other way to assert it
    /// did not drop the wall-clock or token limits along with it.
    pub fn extra_limit_count(&self) -> usize {
        self.extra_limits.len()
    }

    /// This budget with a different turn cap, keeping every extra limit.
    ///
    /// For callers that take a configured ceiling and adjust only the turns — a schedule that
    /// declares its own `max_turns`, say. `extra_limits` is private, so struct-update syntax is
    /// not available outside this crate, and rebuilding with `Budget::new` would silently drop
    /// wall-clock and token limits an operator had set.
    pub fn with_max_turns(mut self, max_turns: u32) -> Self {
        self.max_turns = max_turns;
        self
    }

    /// Add an arbitrary [`ResourceLimit`] to this budget, checked every turn alongside the turn
    /// cap. Chainable: `Budget::new(4).with_limit(WallClockLimit(...)).with_limit(TokenLimit(...))`.
    pub fn with_limit(mut self, limit: impl ResourceLimit + 'static) -> Self {
        // `Arc::get_mut` (not `make_mut`, which needs `T: Clone` — trait objects don't support
        // that generically) succeeds whenever this is the only reference, true for every real
        // call site (builder chains are used immediately: `Budget::new(4).with_wall_clock(...)`).
        // The `None` arm is only reachable if a `Budget` were cloned mid-chain before finishing —
        // doesn't happen anywhere in this codebase, but falls back to starting a fresh list
        // rather than panicking if it ever did.
        match Arc::get_mut(&mut self.extra_limits) {
            Some(limits) => limits.push(Box::new(limit)),
            None => self.extra_limits = Arc::new(vec![Box::new(limit)]),
        }
        self
    }

    /// Shorthand for `with_limit(WallClockLimit(max))`.
    pub fn with_wall_clock(self, max: std::time::Duration) -> Self {
        self.with_limit(WallClockLimit(max))
    }

    /// Shorthand for `with_limit(TokenLimit(max_tokens))`.
    pub fn with_token_limit(self, max_tokens: u64) -> Self {
        self.with_limit(TokenLimit(max_tokens))
    }

    /// The name of the first exhausted extra limit (wall-clock, tokens, ...), if any — `None`
    /// means none of the *extra* limits are exhausted (the turn cap is checked separately, since
    /// it's the loop's own mechanical bound, not one of these).
    pub(crate) fn exhausted_extra(&self, usage: &ResourceUsage) -> Option<&'static str> {
        self.extra_limits
            .iter()
            .find(|limit| limit.is_exhausted(usage))
            .map(|limit| limit.name())
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_TURNS)
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn proptest_exhaustion_monotone(
            (turns1, turns2) in (0u32..50, 0u32..50),
            (elapsed1, elapsed2) in (0u64..300, 0u64..300),
            (tokens1, tokens2) in (0u64..50000, 0u64..50000),
            max_elapsed in 1u64..300,
            max_tokens in 1u64..50000,
        ) {
            if turns1 <= turns2 && elapsed1 <= elapsed2 && tokens1 <= tokens2 {
                let u1 = ResourceUsage {
                    turns: turns1,
                    elapsed: std::time::Duration::from_secs(elapsed1),
                    tokens: tokens1,
                };
                let u2 = ResourceUsage {
                    turns: turns2,
                    elapsed: std::time::Duration::from_secs(elapsed2),
                    tokens: tokens2,
                };
                let wall1 = WallClockLimit(std::time::Duration::from_secs(max_elapsed))
                    .is_exhausted(&u1);
                let wall2 = WallClockLimit(std::time::Duration::from_secs(max_elapsed))
                    .is_exhausted(&u2);
                let tok1 = TokenLimit(max_tokens).is_exhausted(&u1);
                let tok2 = TokenLimit(max_tokens).is_exhausted(&u2);
                if wall1 {
                    prop_assert!(wall2);
                }
                if tok1 {
                    prop_assert!(tok2);
                }
            }
        }

        #[test]
        fn proptest_wall_clock_no_panic(
            elapsed_secs in 0u64..,
            limit_secs in 0u64..,
        ) {
            let usage = ResourceUsage {
                elapsed: std::time::Duration::from_secs(elapsed_secs),
                ..Default::default()
            };
            let _ = WallClockLimit(std::time::Duration::from_secs(limit_secs))
                .is_exhausted(&usage);
        }

        #[test]
        fn proptest_token_limit_no_panic(
            tokens in 0u64..,
            limit in 0u64..,
        ) {
            let usage = ResourceUsage {
                tokens,
                ..Default::default()
            };
            let _ = TokenLimit(limit).is_exhausted(&usage);
        }
    }
}
