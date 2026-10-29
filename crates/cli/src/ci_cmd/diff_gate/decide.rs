//! Apply the new-function limits and the legacy "must not get worse" rule.
//!
//! A score at or under the limit passes. A legacy score over the limit passes when it is
//! not strictly greater than the merge-base score. CRAP uses [`super::CRAP_EPSILON`].
//! Cyclomatic uses [`super::CYCLOMATIC_EPSILON`].

use super::classify::Classified;
use super::model::{
    Judgement, Limits, MetricJudgement, MetricKind, ScoredFn, strictly_worse, within_limit,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetricSet {
    Cyclomatic,
    Crap,
    Both,
}

pub struct Outcome {
    pub judgements: Vec<Judgement>,
    pub needs_base_crap: bool,
}

pub fn decide(
    head: &[ScoredFn],
    base: &[ScoredFn],
    classified: &[Classified],
    limits: Limits,
    metrics: MetricSet,
) -> Outcome {
    let mut judgements = Vec::new();
    for item in classified {
        let Some(score) = head.get(item.head_index) else {
            continue;
        };
        let base_score = item.base_index.and_then(|index| base.get(index));
        let mut metric_rows = Vec::new();
        if metrics != MetricSet::Crap {
            metric_rows.push(judge(
                MetricKind::Cyclomatic,
                score.cyclomatic,
                base_score.map(|entry| entry.cyclomatic),
                limits.cyclomatic,
                item.class.is_legacy(),
            ));
        }
        if metrics != MetricSet::Cyclomatic
            && let Some(current) = score.crap
        {
            metric_rows.push(judge(
                MetricKind::Crap,
                current,
                base_score.and_then(|entry| entry.crap),
                limits.crap,
                item.class.is_legacy(),
            ));
        }
        judgements.push(Judgement {
            file: score.file.clone(),
            function: score.function.clone(),
            line: score.line,
            class: item.class.clone(),
            metrics: metric_rows,
        });
    }
    let needs_base_crap = needs_measured_base_crap(&judgements);
    Outcome {
        judgements,
        needs_base_crap,
    }
}

pub fn needs_measured_base_crap(judgements: &[Judgement]) -> bool {
    judgements.iter().any(legacy_crap_missing_base)
}

fn legacy_crap_missing_base(judgement: &Judgement) -> bool {
    if !judgement.class.is_legacy() {
        return false;
    }
    judgement.metrics.iter().any(|metric| {
        metric.kind == MetricKind::Crap
            && metric.base.is_none()
            && !within_limit(metric.current, metric.limit, metric.kind.epsilon())
    })
}

fn judge(
    kind: MetricKind,
    current: f64,
    base: Option<f64>,
    limit: f64,
    legacy: bool,
) -> MetricJudgement {
    let epsilon = kind.epsilon();
    let failed = if within_limit(current, limit, epsilon) {
        false
    } else if !legacy {
        true
    } else {
        base.is_none_or(|score| strictly_worse(current, score, epsilon))
    };
    MetricJudgement {
        kind,
        current,
        base,
        limit,
        failed,
    }
}

pub fn failures(judgements: &[Judgement]) -> Vec<&Judgement> {
    judgements.iter().filter(|item| item.failed()).collect()
}

#[cfg(test)]
mod tests {
    use super::{MetricSet, decide};
    use crate::ci_cmd::diff_gate::classify::Classified;
    use crate::ci_cmd::diff_gate::model::{Class, Limits, MetricKind, ScoredFn};

    fn limits() -> Limits {
        Limits {
            crap: 29.9,
            cyclomatic: 20.0,
        }
    }

    fn scored(cyclo: f64, crap: f64) -> ScoredFn {
        ScoredFn {
            file: "a.rs".into(),
            function: "work".into(),
            line: 10,
            end_line: 12,
            cyclomatic: cyclo,
            crap: Some(crap),
            body: String::new(),
        }
    }

    fn classified(class: Class, base_index: Option<usize>) -> Classified {
        Classified {
            head_index: 0,
            class,
            base_index,
        }
    }

    fn failed(
        kind: MetricKind,
        head: &ScoredFn,
        base: Option<&ScoredFn>,
        item: &Classified,
    ) -> bool {
        let none: [ScoredFn; 0] = [];
        let base_rows = base.map_or(&none[..], std::slice::from_ref);
        let outcome = decide(
            std::slice::from_ref(head),
            base_rows,
            std::slice::from_ref(item),
            limits(),
            MetricSet::Both,
        );
        outcome.judgements[0]
            .metrics
            .iter()
            .any(|metric| metric.kind == kind && metric.failed)
    }

    #[test]
    fn a_new_function_must_meet_both_limits() {
        let under = scored(20.0, 29.9);
        let fresh = classified(Class::New, None);
        assert!(!failed(MetricKind::Cyclomatic, &under, None, &fresh));
        let over_cyclo = scored(21.0, 1.0);
        assert!(failed(MetricKind::Cyclomatic, &over_cyclo, None, &fresh));
        let over_crap = scored(1.0, 30.0);
        assert!(failed(MetricKind::Crap, &over_crap, None, &fresh));
    }

    #[test]
    fn legacy_within_the_limit_passes_even_when_the_score_rose() {
        let head = scored(10.0, 12.0);
        let base = scored(4.0, 4.0);
        let item = classified(Class::Legacy, Some(0));
        assert!(!failed(MetricKind::Cyclomatic, &head, Some(&base), &item));
        assert!(!failed(MetricKind::Crap, &head, Some(&base), &item));
    }

    #[test]
    fn legacy_over_the_limit_may_improve_or_hold_and_must_not_rise() {
        let item = classified(Class::Legacy, Some(0));
        let base = scored(22.0, 40.0);
        let improved = scored(22.0, 35.0);
        let held = scored(22.0, 40.0);
        let worse_crap = scored(22.0, 41.0);
        let worse_cyclo = scored(23.0, 40.0);
        assert!(!failed(MetricKind::Crap, &improved, Some(&base), &item));
        assert!(!failed(MetricKind::Crap, &held, Some(&base), &item));
        assert!(failed(MetricKind::Crap, &worse_crap, Some(&base), &item));
        assert!(!failed(MetricKind::Cyclomatic, &held, Some(&base), &item));
        assert!(failed(
            MetricKind::Cyclomatic,
            &worse_cyclo,
            Some(&base),
            &item
        ));
    }

    #[test]
    fn a_crap_rise_inside_the_epsilon_is_not_worse() {
        let item = classified(Class::Legacy, Some(0));
        let base = scored(30.0, 40.0);
        let tiny = scored(30.0, 40.001);
        assert!(!failed(MetricKind::Crap, &tiny, Some(&base), &item));
        let real = scored(30.0, 40.002);
        assert!(failed(MetricKind::Crap, &real, Some(&base), &item));
    }

    #[test]
    fn an_over_limit_legacy_function_asks_for_base_crap_only_when_it_is_missing() {
        let over = scored(30.0, 40.0);
        let missing = classified(Class::Legacy, None);
        let outcome = decide(
            std::slice::from_ref(&over),
            &[],
            std::slice::from_ref(&missing),
            limits(),
            MetricSet::Both,
        );
        assert!(outcome.needs_base_crap);
        let present = classified(Class::Legacy, Some(0));
        let held = decide(
            std::slice::from_ref(&over),
            std::slice::from_ref(&over),
            std::slice::from_ref(&present),
            limits(),
            MetricSet::Both,
        );
        assert!(!held.needs_base_crap);
        let under = scored(2.0, 2.0);
        let quiet = decide(
            &[under],
            &[],
            &[classified(Class::Legacy, None)],
            limits(),
            MetricSet::Both,
        );
        assert!(!quiet.needs_base_crap);
    }

    #[test]
    fn cyclomatic_only_does_not_ask_for_base_crap() {
        let over = scored(30.0, 40.0);
        let outcome = decide(
            &[over],
            &[],
            &[classified(Class::Legacy, None)],
            limits(),
            MetricSet::Cyclomatic,
        );
        assert!(!outcome.needs_base_crap);
        assert!(
            outcome.judgements[0]
                .metrics
                .iter()
                .all(|metric| metric.kind == MetricKind::Cyclomatic)
        );
    }
}
