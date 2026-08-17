//! Per-path result shapes (stdout JSON + vault report).

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathId {
    P1a,
    P1b,
    P2,
    P3,
    P4,
    P5,
    /// Durable turn outlives connection + attach + cancel rollback (parallel deliverable §5).
    P6,
    /// Chat turn honest across daemon restart (round-2 §5 / Tier 3 P7). Opt-in: needs restart hook.
    P7,
}

impl PathId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::P1a => "p1a",
            Self::P1b => "p1b",
            Self::P2 => "p2",
            Self::P3 => "p3",
            Self::P4 => "p4",
            Self::P5 => "p5",
            Self::P6 => "p6",
            Self::P7 => "p7",
        }
    }

    /// Non-advisory paths set the exit code by default. P5 is model-decision flaky.
    pub fn is_advisory(self) -> bool {
        matches!(self, Self::P5)
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "p1a" | "1a" => Some(Self::P1a),
            "p1b" | "1b" => Some(Self::P1b),
            "p2" | "2" => Some(Self::P2),
            "p3" | "3" => Some(Self::P3),
            "p4" | "4" => Some(Self::P4),
            "p5" | "5" => Some(Self::P5),
            "p6" | "6" => Some(Self::P6),
            "p7" | "7" => Some(Self::P7),
            _ => None,
        }
    }

    /// Paths run when `paths` is empty in config / no CLI filter.
    ///
    /// **Default-set decision (explicit):**
    /// - **P1a–P4**: always on — schedule/hook/spawn/join ground truth; no restart side effects.
    /// - **P5** (delegate): **opt-in** — advisory / model-flaky; use `paths` or `advisory_counts`.
    /// - **P6** (durable turn): **opt-in** — two real-inference background turns; gating when
    ///   selected (`p6` / listed) but not in the plain suite so a default run stays cheap.
    /// - **P7** (restart survival): **opt-in** — restarts the daemon via config hook; never in
    ///   default so a conformance run cannot reboot the box by surprise. Unconfigured hook skips.
    pub fn all_default() -> Vec<Self> {
        vec![Self::P1a, Self::P1b, Self::P2, Self::P3, Self::P4]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathStatus {
    Pass,
    Fail,
    Skipped,
}

#[derive(Debug, Clone, Serialize)]
pub struct PathResult {
    pub path: String,
    pub status: PathStatus,
    pub duration_ms: u64,
    pub assertion: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Whether this path is advisory (does not fail the run by default).
    #[serde(default)]
    pub advisory: bool,
}

impl PathResult {
    pub fn pass(
        id: PathId,
        assertion: impl Into<String>,
        duration_ms: u64,
        evidence: serde_json::Value,
    ) -> Self {
        Self {
            path: id.as_str().into(),
            status: PathStatus::Pass,
            duration_ms,
            assertion: assertion.into(),
            evidence: Some(evidence),
            reason: None,
            advisory: id.is_advisory(),
        }
    }

    pub fn fail(
        id: PathId,
        assertion: impl Into<String>,
        duration_ms: u64,
        evidence: serde_json::Value,
    ) -> Self {
        Self {
            path: id.as_str().into(),
            status: PathStatus::Fail,
            duration_ms,
            assertion: assertion.into(),
            evidence: Some(evidence),
            reason: None,
            advisory: id.is_advisory(),
        }
    }

    pub fn skipped(id: PathId, reason: impl Into<String>) -> Self {
        Self {
            path: id.as_str().into(),
            status: PathStatus::Skipped,
            duration_ms: 0,
            assertion: String::new(),
            evidence: None,
            reason: Some(reason.into()),
            advisory: id.is_advisory(),
        }
    }

    pub fn is_blocking_fail(&self, advisory_counts: bool) -> bool {
        self.status == PathStatus::Fail && (advisory_counts || !self.advisory)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    pub started_at: String,
    pub finished_at: String,
    pub overall: PathStatus,
    pub base_url: String,
    pub results: Vec<PathResult>,
}

impl RunReport {
    pub fn compute_overall(results: &[PathResult], advisory_counts: bool) -> PathStatus {
        if results.iter().any(|r| r.is_blocking_fail(advisory_counts)) {
            PathStatus::Fail
        } else if results.iter().all(|r| r.status == PathStatus::Skipped) {
            PathStatus::Skipped
        } else {
            PathStatus::Pass
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advisory_fail_does_not_block_by_default() {
        let r = PathResult::fail(PathId::P5, "delegate", 1, serde_json::json!({}));
        assert!(r.advisory);
        assert!(!r.is_blocking_fail(false));
        assert!(r.is_blocking_fail(true));
    }

    #[test]
    fn non_advisory_fail_blocks() {
        let r = PathResult::fail(PathId::P1b, "artifact", 1, serde_json::json!({}));
        assert!(r.is_blocking_fail(false));
    }

    #[test]
    fn p6_is_registered_and_not_advisory() {
        assert_eq!(PathId::parse("p6"), Some(PathId::P6));
        assert_eq!(PathId::P6.as_str(), "p6");
        assert!(!PathId::P6.is_advisory());
        let r = PathResult::fail(PathId::P6, "durable", 1, serde_json::json!({}));
        assert!(r.is_blocking_fail(false));
    }

    #[test]
    fn p7_is_registered_not_advisory_and_not_in_default_set() {
        assert_eq!(PathId::parse("p7"), Some(PathId::P7));
        assert_eq!(PathId::parse("7"), Some(PathId::P7));
        assert_eq!(PathId::P7.as_str(), "p7");
        assert!(!PathId::P7.is_advisory());
        assert!(
            !PathId::all_default().contains(&PathId::P7),
            "P7 must stay opt-in (restart side effect)"
        );
        assert!(
            !PathId::all_default().contains(&PathId::P6),
            "P6 stays opt-in (real inference cost)"
        );
        let r = PathResult::fail(PathId::P7, "restart", 1, serde_json::json!({}));
        assert!(r.is_blocking_fail(false));
    }

    /// A suite of only skips is overall Skipped — never Pass (skip ≠ pass).
    #[test]
    fn all_skipped_is_overall_skipped_not_pass() {
        let results = vec![
            PathResult::skipped(PathId::P7, "restart_command unset"),
            PathResult::skipped(PathId::P3, "no secret"),
        ];
        assert_eq!(
            RunReport::compute_overall(&results, false),
            PathStatus::Skipped
        );
        assert_ne!(
            RunReport::compute_overall(&results, false),
            PathStatus::Pass
        );
    }

    #[test]
    fn skip_is_not_counted_as_pass_status() {
        let r = PathResult::skipped(PathId::P7, "no restart hook configured");
        assert_eq!(r.status, PathStatus::Skipped);
        assert!(r.reason.as_ref().is_some_and(|s| !s.is_empty()));
        assert!(!r.is_blocking_fail(false));
    }

    /// Every `PathId::parse` arm must stay reachable — a deleted arm is a silently unknown path.
    #[test]
    fn parse_accepts_every_alias() {
        for (alias, id) in [
            ("p1a", PathId::P1a),
            ("1a", PathId::P1a),
            ("p1b", PathId::P1b),
            ("1b", PathId::P1b),
            ("p2", PathId::P2),
            ("2", PathId::P2),
            ("p3", PathId::P3),
            ("3", PathId::P3),
            ("p4", PathId::P4),
            ("4", PathId::P4),
            ("p5", PathId::P5),
            ("5", PathId::P5),
            ("p6", PathId::P6),
            ("6", PathId::P6),
            ("p7", PathId::P7),
            ("7", PathId::P7),
        ] {
            assert_eq!(PathId::parse(alias), Some(id), "alias {alias}");
            assert_eq!(
                PathId::parse(&alias.to_uppercase()),
                Some(id),
                "alias {alias}"
            );
        }
        assert_eq!(PathId::parse("p8"), None);
        assert_eq!(PathId::parse(""), None);
    }

    /// The default set is exactly the five non-advisory, side-effect-free paths.
    #[test]
    fn default_set_is_exactly_p1a_through_p4() {
        assert_eq!(
            PathId::all_default(),
            vec![PathId::P1a, PathId::P1b, PathId::P2, PathId::P3, PathId::P4]
        );
    }
}
