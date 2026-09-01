//! Declarative risk-guard waivers.
//!
//! A waiver suppresses a specific guard for tool calls that match a (MCP, tools, zones)
//! triple. It is **not** an authority grant — the [`crate::capability::CapabilitySet`] still
//! decides whether a call is permitted; the waiver only relaxes a *further* gate (today:
//! the magnitude heuristic) that would otherwise false-positive on read-heavy goals.
//!
//! The shape was chosen to mirror the existing grant vocabulary (`Capability`, `Zone`,
//! `CapabilitySet`) so the operator's mental model carries over: a waiver is "a thing the
//! policy says is allowed to skip a guard", not a thing the policy says is allowed to run.
//! A waiver can never widen authority; if a call is not in the active grant, the capability
//! guard catches it before any waiver is consulted.
//!
//! Design notes:
//!
//! * A waiver matches a *call* by three checks, all of which must hold:
//!   1. `mcp` matches the MCP the call would invoke.
//!   2. Either `match_tools` is `None` (wildcard) or the bare tool name is in it.
//!   3. Either `match_zones` is `None` (wildcard) or the call's resolved target zone is in
//!      it. A call whose zone cannot be resolved (`WriteTarget::NotAWrite`, or
//!      `Undeterminable`) does not match a zone-restricted waiver — operators who list
//!      specific zones want the match to be on a real zone, not on "no zone".
//! * `guard` names the gate to suppress. Today only [`Guard::Magnitude`] is meaningful;
//!   adding more is a deliberate, opt-in change so a typo can't silently disable a guard.
//! * Validation is the loader's job, not this module's: a waiver referencing an unknown
//!   MCP or undeclared zone is a load-time error (`crates/config-loader/src/validation.rs`).
//!   This module trusts its input.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::catalog::WriteTarget;
use crate::dispatch::{bare_tool_name, mcp_of};

/// A guard the waiver pipeline knows how to suppress.
///
/// The set is deliberately small. Each new variant is a load-time opt-in: a typo in
/// `policy.toml` (`guard = "magnitde"`) is refused at parse, not silently dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Guard {
    /// The sweeping-destructive heuristic in
    /// [`crate::capability::is_sweeping_destructive`]. The most common false-positive
    /// source: a goal that mentions sweeping words (`everything`, `all`) in the service
    /// of a small, scoped edit ("keep everything else exactly as-is") trips the gate on
    /// legitimate read or rewrite work.
    Magnitude,
}

impl Guard {
    /// Every defined guard, for validation/iteration. Add new variants here when adding
    /// them to the enum so a config that lists them passes validation.
    pub const ALL: &'static [Guard] = &[Guard::Magnitude];
}

/// A single waiver entry — parsed from `[[risk_waivers]]` in `policy.toml`.
///
/// Constructed via `Deserialize` from the config; the loader validates that `mcp` and
/// any `match_zones` resolve against the rest of the loaded config.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskWaiver {
    /// The MCP the call must target. Required. Matches by exact string equality against
    /// the MCP name (the prefix of `"<mcp>:<tool>"`).
    pub mcp: String,
    /// Optional list of bare tool names (the suffix of `"<mcp>:<tool>"`) the waiver
    /// covers. `None` or empty = every tool on the MCP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_tools: Option<Vec<String>>,
    /// Optional list of zone names the waiver covers. `None` or empty = every zone.
    /// Matched against the call's resolved target zone (see [`Self::covers`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_zones: Option<Vec<String>>,
    /// Which guard this waiver suppresses. Required.
    pub guard: Guard,
}

impl RiskWaiver {
    /// Whether this waiver covers a call to `tool` (a `"<mcp>:<tool>"` name) targeting
    /// `zone` (the call's resolved zone — `None` when not a vault write or zone unknown).
    ///
    /// The check is purely structural: it does not look at arguments, prompts, or
    /// capability grants. A waiver is a config-time assertion that for these MCP+tool+zone
    /// combinations the named guard adds no safety; the runtime still enforces authority
    /// upstream.
    pub fn covers(&self, tool: &str, zone: Option<&str>) -> bool {
        if mcp_of(tool) != self.mcp {
            return false;
        }
        if let Some(tools) = &self.match_tools
            && !tools.is_empty()
            && !tools.iter().any(|t| t == bare_tool_name(tool))
        {
            return false;
        }
        if let Some(zones) = &self.match_zones
            && !zones.is_empty()
        {
            // A zone-restricted waiver only matches when the call has a concrete zone to
            // compare. "No zone" (a read, or a write with an unresolved path) does not
            // accidentally match a list of specific zones.
            let Some(call_zone) = zone else {
                return false;
            };
            if !zones.iter().any(|z| z == call_zone) {
                return false;
            }
        }
        true
    }
}

/// A `RiskWaiverSet` is the resolved, validated set of waivers at runtime — loaded once
/// from policy and read by every guard pipeline that needs to check coverage.
///
/// The inner type is `BTreeSet<RiskWaiver>` so duplicate declarations in `policy.toml`
/// collapse (a typo'd double entry doesn't double-suppress). Order is irrelevant for
/// `covers_any`; the set is the right shape.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskWaiverSet {
    pub waivers: BTreeSet<RiskWaiver>,
}

impl RiskWaiverSet {
    /// A set with no waivers. The magnitude heuristic fires as it did before this
    /// feature shipped.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Whether *any* waiver in this set covers `tool` (a `"<mcp>:<tool>"` name) for the
    /// given `guard`, where the call's resolved target zone is `zone`.
    pub fn covers(&self, guard: Guard, tool: &str, zone: Option<&str>) -> bool {
        self.waivers
            .iter()
            .any(|w| w.guard == guard && w.covers(tool, zone))
    }
}

/// The input to a magnitude-waiver check at the dispatch boundary. Bundles the
/// per-call resolution the dispatcher already does (MCP + tool + resolved zone) so the
/// guard pipeline can ask one question instead of re-resolving the catalog.
///
/// `zone` is the call's resolved target zone (the leading path segment of a path-addressed
/// write, or the tool's declared zone for a fixed-zone MCP, or `None` for anything else).
#[derive(Debug, Clone, Copy)]
pub struct WaiverTarget<'a> {
    /// The `"<mcp>:<tool>"` qualified name the guard pipeline is checking.
    pub qualified_tool: &'a str,
    /// The resolved target zone, or `None` if the call has no determinable zone (a read
    /// on a non-vault MCP, or a write to a path that doesn't name a zone).
    pub zone: Option<&'a str>,
}

impl<'a> WaiverTarget<'a> {
    /// Build a target from a `"<mcp>:<tool>"` name and its `WriteTarget`.
    ///
    /// Only `WriteTarget::Zone(name)` produces a non-`None` zone; `NotAWrite` and
    /// `Undeterminable` both map to `None` so a zone-restricted waiver does not match.
    pub fn from_write_target(qualified_tool: &'a str, target: &'a WriteTarget) -> Self {
        let zone = match target {
            WriteTarget::Zone(name) => Some(name.as_str()),
            WriteTarget::NotAWrite | WriteTarget::Undeterminable(_) => None,
        };
        Self {
            qualified_tool,
            zone,
        }
    }
}

impl RiskWaiverSet {
    /// The single question the dispatcher asks: "is every (tool, zone) this action would
    /// touch covered by a magnitude waiver?"
    ///
    /// Returns `true` (the magnitude gate is suppressed) when **every** target is covered
    /// AND the waiver list is non-empty. An empty set is never "everything is waived" —
    /// it is the unchanged default behaviour — so the caller doesn't have to special-case
    /// the empty-input case beyond passing it through.
    ///
    /// A target whose zone cannot be resolved is treated as "not covered" by a
    /// zone-restricted waiver (see [`RiskWaiver::covers`]); the caller is expected to
    /// pass the resolved zone, not a wildcard.
    pub fn all_magnitude_waived<'a, I>(&self, targets: I) -> bool
    where
        I: IntoIterator<Item = WaiverTarget<'a>>,
    {
        let mut iter = targets.into_iter();
        let Some(first) = iter.next() else {
            // A waiver must cover a concrete target. Treating an empty target list as waived
            // would suppress the guard when the classifier did not identify any MCP or tool.
            return false;
        };
        let first_ok = self.covers(Guard::Magnitude, first.qualified_tool, first.zone);
        if !first_ok {
            return false;
        }
        iter.all(|t| self.covers(Guard::Magnitude, t.qualified_tool, t.zone))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::WriteTarget;

    fn w(
        mcp: &str,
        tools: Option<Vec<&str>>,
        zones: Option<Vec<&str>>,
        guard: Guard,
    ) -> RiskWaiver {
        RiskWaiver {
            mcp: mcp.into(),
            match_tools: tools.map(|t| t.into_iter().map(String::from).collect()),
            match_zones: zones.map(|z| z.into_iter().map(String::from).collect()),
            guard,
        }
    }

    #[test]
    fn covers_matches_when_no_filters() {
        let waiver = w("liberado-weather-mcp", None, None, Guard::Magnitude);
        assert!(waiver.covers("liberado-weather-mcp:get_weather", None));
        assert!(waiver.covers("liberado-weather-mcp:anything", Some("Tasks")));
    }

    #[test]
    fn covers_filters_by_tool() {
        let waiver = w(
            "turbovault",
            Some(vec!["read_note", "search"]),
            None,
            Guard::Magnitude,
        );
        assert!(waiver.covers("turbovault:read_note", None));
        assert!(waiver.covers("turbovault:search", Some("Tasks")));
        assert!(!waiver.covers("turbovault:write_note", Some("Tasks")));
    }

    #[test]
    fn covers_filters_by_zone() {
        let waiver = w(
            "turbovault",
            None,
            Some(vec!["Tasks", "Work"]),
            Guard::Magnitude,
        );
        assert!(waiver.covers("turbovault:write_note", Some("Tasks")));
        assert!(waiver.covers("turbovault:read_note", Some("Work")));
        assert!(!waiver.covers("turbovault:write_note", Some("Journal")));
        // A call with no resolved zone does not match a zone-restricted waiver.
        assert!(!waiver.covers("turbovault:read_note", None));
    }

    #[test]
    fn covers_requires_mcp_match() {
        let waiver = w("turbovault", None, None, Guard::Magnitude);
        assert!(!waiver.covers("liberado-weather-mcp:get_weather", None));
    }

    #[test]
    fn empty_filter_lists_are_wildcards() {
        let waiver = w("turbovault", Some(vec![]), Some(vec![]), Guard::Magnitude);
        assert!(waiver.covers("turbovault:read_note", None));
        assert!(waiver.covers("turbovault:write_note", Some("Tasks")));
    }

    #[test]
    fn waiver_target_from_write_target_zone() {
        let zone_target = WriteTarget::Zone("Tasks".into());
        let t = WaiverTarget::from_write_target("turbovault:write_note", &zone_target);
        assert_eq!(t.zone, Some("Tasks"));
        assert_eq!(t.qualified_tool, "turbovault:write_note");
    }

    #[test]
    fn waiver_target_from_write_target_read_or_undeterminable_has_no_zone() {
        let read_target = WriteTarget::NotAWrite;
        let read = WaiverTarget::from_write_target("turbovault:read_note", &read_target);
        assert!(read.zone.is_none());
        let und_target = WriteTarget::Undeterminable("no path".into());
        let und = WaiverTarget::from_write_target("turbovault:write_note", &und_target);
        assert!(und.zone.is_none());
    }

    #[test]
    fn all_magnitude_waived_requires_every_target_covered() {
        let set = RiskWaiverSet {
            waivers: [w(
                "turbovault",
                Some(vec!["read_note"]),
                None,
                Guard::Magnitude,
            )]
            .into_iter()
            .collect(),
        };
        let read_only = [
            WaiverTarget {
                qualified_tool: "turbovault:read_note",
                zone: None,
            },
            WaiverTarget {
                qualified_tool: "turbovault:read_note",
                zone: Some("Tasks"),
            },
        ];
        assert!(set.all_magnitude_waived(read_only));

        // A write in the same action is not covered.
        let mixed = [
            WaiverTarget {
                qualified_tool: "turbovault:read_note",
                zone: Some("Tasks"),
            },
            WaiverTarget {
                qualified_tool: "turbovault:write_note",
                zone: Some("Tasks"),
            },
        ];
        assert!(!set.all_magnitude_waived(mixed));
    }

    #[test]
    fn empty_set_waives_nothing() {
        let set = RiskWaiverSet::empty();
        let read = [WaiverTarget {
            qualified_tool: "turbovault:read_note",
            zone: None,
        }];
        assert!(!set.all_magnitude_waived(read));
    }

    #[test]
    fn empty_target_list_is_not_waived() {
        let set = RiskWaiverSet {
            waivers: [w("weather", None, None, Guard::Magnitude)]
                .into_iter()
                .collect(),
        };
        assert!(!set.all_magnitude_waived([]));
    }

    #[test]
    fn duplicate_waivers_collapse_to_one() {
        let set = RiskWaiverSet {
            waivers: [
                w("weather", None, None, Guard::Magnitude),
                w("weather", None, None, Guard::Magnitude),
            ]
            .into_iter()
            .collect(),
        };
        assert_eq!(set.waivers.len(), 1);
    }

    #[test]
    fn unknown_guard_variant_in_toml_is_a_deserialize_error() {
        // Sanity check on `#[serde(rename_all = "snake_case")]` — a typo'd guard name
        // would otherwise silently fall through to no waiver at all. (The Guard enum has
        // no fall-through; serde is expected to reject unknown variants.)
        let bad = r#"
mcp = "weather"
guard = "magnitde"
"#;
        let parsed: Result<RiskWaiver, _> = toml::from_str(bad);
        assert!(parsed.is_err(), "a typo'd guard name must fail to parse");
    }
}
