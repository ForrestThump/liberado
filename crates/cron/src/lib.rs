//! # liberado-cron
//!
//! Cron as an [`EventSource`] (Decision 18/19): the second conformer to the seam
//! `liberado-daemon`'s `VaultEventSource` proved first, and the literal proof of "cron and
//! vault-watch are interchangeable event-sources" (Decision 18 checkpoint #3). A [`Schedule`]
//! fires on its own timer and produces the same standardized `Event` a vault change does — the
//! daemon reacts to it through the exact same `react()` path, never knowing or caring it came from
//! a clock instead of a file change.
//!
//! Deliberately vault-agnostic: this crate has no `liberado-vault` dependency at all, which is the
//! concrete proof Decision 19's "the core is vault-agnostic" claim is real, not aspirational.
//!
//! **No catch-up/backfill on restart**: a schedule due while the daemon was down simply doesn't
//! fire retroactively — ordinary cron semantics, not a regression from anything that exists today.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use liberado_common::{Event, EventPayload, EventSource, event_source};
use tokio::sync::mpsc::UnboundedSender;

/// A configured cron schedule: fire on `cron_expr` and dispatch `goal` when it does. `cron_expr`
/// uses the `cron` crate's 6/7-field syntax (**seconds first** — not standard 5-field cron), e.g.
/// `"0 0 9 * * * *"` for "every day at 09:00:00".
#[derive(Debug, Clone)]
pub struct Schedule {
    pub name: String,
    pub cron_expr: String,
    pub goal: String,
    /// Which named dispatcher/executor pool (Decision 18 checkpoint #3) handles this schedule's
    /// firing — `None` routes to the daemon's always-present `"default"` pool.
    pub pool: Option<String>,
    /// Optional session profile name (E7) — carried into the event so the hub session can resolve
    /// a grant that may include `AskHuman` / a long idle budget.
    pub profile: Option<String>,
    /// Whether this schedule's result is pushed to the notifier when it finishes.
    ///
    /// `None` keeps today's behaviour (deliver). `Some(false)` silences it: a maintenance schedule
    /// that runs hourly and usually finds nothing to do otherwise posts 24 messages a day, and the
    /// only alternative was asking the goal prompt politely to be brief — prompt-level etiquette
    /// standing in for a config flag.
    pub deliver: Option<bool>,
}

/// Errors constructing a [`CronEventSource`] — both fail-fast at construction time (Decision 14's
/// spirit), never discovered only once a schedule was due to fire.
#[derive(Debug, thiserror::Error)]
pub enum CronError {
    #[error("schedule '{name}' has an invalid cron expression '{expr}': {source}")]
    InvalidExpression {
        name: String,
        expr: String,
        #[source]
        source: cron::error::Error,
    },
    #[error("duplicate schedule name '{0}'")]
    DuplicateName(String),
}

/// One parsed, ready-to-fire schedule.
#[derive(Debug)]
struct ParsedSchedule {
    name: String,
    goal: String,
    pool: Option<String>,
    profile: Option<String>,
    deliver: Option<bool>,
    parsed: cron::Schedule,
}

/// The cron [`EventSource`]: wakes for whichever attached schedule fires next, produces an
/// [`Event`] carrying that schedule's `goal`, repeats. Multiple schedules share one timer loop
/// (not one task each) — simpler, and firings are still independent since each schedule keeps its
/// own next-fire time in the heap.
#[derive(Debug)]
pub struct CronEventSource {
    schedules: Vec<ParsedSchedule>,
}

impl CronEventSource {
    /// Parse every schedule's cron expression up front and reject duplicate names — fail fast on a
    /// malformed schedule rather than discovering it only when it was due to fire.
    pub fn new(schedules: Vec<Schedule>) -> Result<Self, CronError> {
        let mut seen = HashSet::new();
        let mut parsed = Vec::with_capacity(schedules.len());
        for s in schedules {
            if !seen.insert(s.name.clone()) {
                return Err(CronError::DuplicateName(s.name));
            }
            let expr = cron::Schedule::from_str(&s.cron_expr).map_err(|source| {
                CronError::InvalidExpression {
                    name: s.name.clone(),
                    expr: s.cron_expr.clone(),
                    source,
                }
            })?;
            parsed.push(ParsedSchedule {
                name: s.name,
                goal: s.goal,
                pool: s.pool,
                profile: s.profile,
                deliver: s.deliver,
                parsed: expr,
            });
        }
        Ok(Self { schedules: parsed })
    }
}

#[async_trait]
impl EventSource for CronEventSource {
    fn name(&self) -> &str {
        "cron"
    }

    async fn run(self: Box<Self>, tx: UnboundedSender<Event>) {
        if self.schedules.is_empty() {
            tracing::info!("cron: no schedules configured, nothing to run");
            return;
        }

        // One next-fire time per schedule, kept in a min-heap (via `Reverse`) so the loop always
        // sleeps exactly until the earliest one, regardless of how many schedules are attached.
        let now = Utc::now();
        let mut heap: BinaryHeap<Reverse<(DateTime<Utc>, usize)>> = self
            .schedules
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.parsed.after(&now).next().map(|next| Reverse((next, i))))
            .collect();

        loop {
            let Some(Reverse((fire_at, idx))) = heap.pop() else {
                tracing::warn!("cron: no schedule has any future occurrence; stopping");
                return;
            };

            let wait = (fire_at - Utc::now())
                .to_std()
                .unwrap_or(std::time::Duration::ZERO);
            tokio::time::sleep(wait).await;

            let schedule = &self.schedules[idx];
            tracing::info!(schedule = %schedule.name, fire_at = %fire_at, "cron schedule fired");
            if tx.send(build_event(schedule, fire_at)).is_err() {
                return; // receiver gone
            }

            // Requeue this schedule's next occurrence.
            if let Some(next) = schedule.parsed.after(&Utc::now()).next() {
                heap.push(Reverse((next, idx)));
            }
        }
    }
}

/// Build the standardized event for a cron firing. `correlation_id` is unique per firing (name +
/// fire time), the idempotency key Decision 6 requires; `source` is `"cron:{name}"` so a consumer
/// can tell which schedule fired without inspecting the payload.
fn build_event(schedule: &ParsedSchedule, fire_at: DateTime<Utc>) -> Event {
    let correlation_id = format!("cron:{}:{}", schedule.name, fire_at.to_rfc3339());
    Event::trigger(
        "CronFired",
        format!("{}:{}", event_source::CRON, schedule.name),
        correlation_id,
        EventPayload {
            summary: Some(schedule.goal.clone()),
            pool: schedule.pool.clone(),
            // The daemon's delivery gate sees only the `Event`, so `deliver` rides here for the
            // same reason `profile` does. Absent means deliver — the pre-existing behaviour.
            data: {
                let mut map = serde_json::Map::new();
                if let Some(p) = &schedule.profile {
                    map.insert("profile".into(), serde_json::Value::String(p.clone()));
                }
                if let Some(d) = schedule.deliver {
                    map.insert("deliver".into(), serde_json::Value::Bool(d));
                }
                if map.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::Object(map)
                }
            },
            ..Default::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule(name: &str, cron_expr: &str, goal: &str) -> Schedule {
        Schedule {
            name: name.into(),
            cron_expr: cron_expr.into(),
            goal: goal.into(),
            pool: None,
            profile: None,
            deliver: None,
        }
    }

    /// `deliver` rides on the event payload because the daemon's delivery gate sees only the
    /// `Event` — if it stopped being carried, the opt-out would silently stop working.
    #[test]
    fn deliver_false_is_carried_on_the_event() {
        let mut s = schedule("quiet", "0 0 * * * * *", "sweep");
        s.deliver = Some(false);
        let parsed = CronEventSource::new(vec![s]).unwrap().schedules;
        let event = build_event(&parsed[0], Utc::now());
        assert_eq!(
            event.payload.data.get("deliver").and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    /// Omitting it must stay indistinguishable from before the flag existed.
    #[test]
    fn absent_deliver_puts_nothing_on_the_event() {
        let parsed = CronEventSource::new(vec![schedule("loud", "0 0 * * * * *", "report")])
            .unwrap()
            .schedules;
        let event = build_event(&parsed[0], Utc::now());
        assert!(
            event.payload.data.get("deliver").is_none(),
            "{:?}",
            event.payload.data
        );
    }

    /// A schedule with both must carry both — the payload map is built once for the pair.
    #[test]
    fn profile_and_deliver_coexist_on_the_event() {
        let mut s = schedule("both", "0 0 * * * * *", "work");
        s.profile = Some("hat".into());
        s.deliver = Some(false);
        let parsed = CronEventSource::new(vec![s]).unwrap().schedules;
        let event = build_event(&parsed[0], Utc::now());
        assert_eq!(
            event.payload.data.get("profile").and_then(|v| v.as_str()),
            Some("hat")
        );
        assert_eq!(
            event.payload.data.get("deliver").and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn a_malformed_cron_expression_is_rejected_at_construction() {
        let err = CronEventSource::new(vec![schedule("bad", "not a cron expr", "goal")])
            .expect_err("malformed expression must fail construction");
        assert!(matches!(err, CronError::InvalidExpression { name, .. } if name == "bad"));
    }

    #[test]
    fn duplicate_schedule_names_are_rejected() {
        let err = CronEventSource::new(vec![
            schedule("nightly", "0 0 0 * * * *", "goal a"),
            schedule("nightly", "0 0 9 * * * *", "goal b"),
        ])
        .expect_err("duplicate names must fail construction");
        assert!(matches!(err, CronError::DuplicateName(name) if name == "nightly"));
    }

    #[test]
    fn a_valid_schedule_constructs_successfully() {
        assert!(CronEventSource::new(vec![schedule("nightly", "0 0 9 * * * *", "goal")]).is_ok());
    }

    #[tokio::test]
    async fn firing_produces_an_event_carrying_the_goal_and_schedule_name() {
        // "every second" — fires almost immediately, so this test doesn't need a long wait.
        let source = CronEventSource::new(vec![schedule(
            "every-second",
            "* * * * * * *",
            "summarize today's decisions",
        )])
        .unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(Box::new(source).run(tx));

        let event = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .expect("timed out waiting for the schedule to fire")
            .expect("channel closed");

        assert_eq!(event.event_type, "CronFired");
        assert_eq!(event.source, "cron:every-second");
        assert_eq!(
            event.payload.summary.as_deref(),
            Some("summarize today's decisions")
        );
        assert!(event.provenance.is_none());
        assert!(event.is_reactable());
    }

    #[tokio::test]
    async fn a_schedules_configured_pool_is_carried_onto_its_event() {
        let mut with_pool = schedule("every-second", "* * * * * * *", "goal");
        with_pool.pool = Some("restricted".to_string());
        let source = CronEventSource::new(vec![with_pool]).unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(Box::new(source).run(tx));

        let event = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .expect("timed out waiting for the schedule to fire")
            .expect("channel closed");

        assert_eq!(event.payload.pool.as_deref(), Some("restricted"));
    }
}
