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
            _ => None,
        }
    }

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
    pub fn pass(id: PathId, assertion: impl Into<String>, duration_ms: u64, evidence: serde_json::Value) -> Self {
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

    pub fn fail(id: PathId, assertion: impl Into<String>, duration_ms: u64, evidence: serde_json::Value) -> Self {
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
        if results
            .iter()
            .any(|r| r.is_blocking_fail(advisory_counts))
        {
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
}
