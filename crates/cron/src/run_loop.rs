//! Cron heap loop: sleep until the next schedule fire, apply catch-up, emit events.

use chrono::{DateTime, Utc};
use liberado_common::Event;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use tokio::sync::mpsc::UnboundedSender;

use super::CronEventSource;
use super::build_event;
use super::catch_up;
use super::fire_at;

impl CronEventSource {
    pub(super) fn seed_mechanical_cursors(&self, now: DateTime<Utc>) {
        let Some(cu) = &self.catch_up else {
            return;
        };
        for schedule in &self.schedules {
            if catch_up::mechanical_reminder(&schedule.job) {
                cu.store.ensure(&schedule.name, now);
            }
        }
    }

    pub(super) async fn drive_heap(
        &self,
        mut heap: BinaryHeap<Reverse<(DateTime<Utc>, usize)>>,
        tx: UnboundedSender<Event>,
    ) {
        loop {
            let Some(Reverse((fire_at, idx))) = heap.pop() else {
                tracing::warn!("cron: no schedule has any future occurrence; stopping");
                return;
            };

            let wait = (fire_at - Utc::now())
                .to_std()
                .unwrap_or(std::time::Duration::ZERO);
            tokio::time::sleep(wait).await;

            if !self.fire_due_schedule(idx, fire_at, &tx, &mut heap).await {
                return;
            }
        }
    }

    async fn fire_due_schedule(
        &self,
        idx: usize,
        fire_at: DateTime<Utc>,
        tx: &UnboundedSender<Event>,
        heap: &mut BinaryHeap<Reverse<(DateTime<Utc>, usize)>>,
    ) -> bool {
        let schedule = &self.schedules[idx];
        let now = Utc::now();
        let (emit_at, is_catch_up) = self.resolve_fire(schedule, fire_at, now);
        if self.should_skip_emit(schedule, emit_at) {
            if let Some(next) = fire_at::next_utc(&schedule.parsed, schedule.zone, now) {
                heap.push(Reverse((next, idx)));
            }
            return true;
        }
        if let Some(cu) = &self.catch_up {
            catch_up::remember_floor(&cu.floor, &schedule.name, emit_at);
        }
        tracing::info!(
            schedule = %schedule.name,
            fire_at = %emit_at,
            catch_up = is_catch_up,
            "cron schedule fired"
        );
        if tx
            .send(build_event(schedule, emit_at, is_catch_up))
            .is_err()
        {
            return false;
        }

        if let Some(next) = fire_at::next_utc(&schedule.parsed, schedule.zone, now) {
            heap.push(Reverse((next, idx)));
        }
        true
    }
}
