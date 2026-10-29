//! Shared scores, limits, and the float epsilons the diff gate compares with.

/// CRAP is worse only when it exceeds the base by more than this.
///
/// 0.001 is above binary rounding noise and below the 0.1 resolution cargo-crap prints.
/// A printed rise of 0.1 is therefore a real rise. A 4→5 move is not special-cased:
/// both scores are under the 29.9 limit, so they pass before this epsilon is consulted.
pub const CRAP_EPSILON: f64 = 0.001;

/// Cyclomatic scores are whole numbers stored as floats. 0.01 keeps `20.0` equal to 20.
pub const CYCLOMATIC_EPSILON: f64 = 0.01;

/// Minimum token-Jaccard similarity that pairs a renamed function with a removed one.
pub const BODY_MATCH_MIN: f64 = 0.8;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub crap: f64,
    pub cyclomatic: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScoredFn {
    pub file: String,
    pub function: String,
    pub line: u32,
    pub end_line: u32,
    pub cyclomatic: f64,
    pub crap: Option<f64>,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FnKey {
    pub file: String,
    pub function: String,
    pub occurrence: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Class {
    New,
    Legacy,
    RenamedFrom { file: String, function: String },
}

impl Class {
    pub fn is_legacy(&self) -> bool {
        !matches!(self, Class::New)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetricKind {
    Crap,
    Cyclomatic,
}

impl MetricKind {
    pub fn epsilon(self) -> f64 {
        match self {
            MetricKind::Crap => CRAP_EPSILON,
            MetricKind::Cyclomatic => CYCLOMATIC_EPSILON,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            MetricKind::Crap => "CRAP",
            MetricKind::Cyclomatic => "cyclomatic",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MetricJudgement {
    pub kind: MetricKind,
    pub current: f64,
    pub base: Option<f64>,
    pub limit: f64,
    pub failed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Judgement {
    pub file: String,
    pub function: String,
    pub line: u32,
    pub class: Class,
    pub metrics: Vec<MetricJudgement>,
}

impl Judgement {
    pub fn failed(&self) -> bool {
        self.metrics.iter().any(|metric| metric.failed)
    }
}

pub fn within_limit(current: f64, limit: f64, epsilon: f64) -> bool {
    current <= limit + epsilon
}

pub fn strictly_worse(current: f64, base: f64, epsilon: f64) -> bool {
    current > base + epsilon
}

pub fn slash(path: &str) -> String {
    path.replace('\\', "/")
}
