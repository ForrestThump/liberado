//! Mechanical job outcome handling — kept off `jobs.rs` for the module-health function count.

use liberado_common::Event;

use crate::types::{Daemon, ReactionOutcome};

use super::jobs::{JobEffect, JobRequest};

impl Daemon {
    pub(super) fn finish_mechanical(
        &self,
        request: &JobRequest,
        event: &Event,
        effect: JobEffect,
    ) -> Option<ReactionOutcome> {
        match effect {
            JobEffect::Continue => None,
            JobEffect::Quiet => {
                tracing::info!(job = %request.name, kind = %request.kind, "mechanical job quiet");
                Some(ReactionOutcome::Observed)
            }
            JobEffect::RemindRows(items) => self.deliver_mechanical_rows(request, event, items),
            JobEffect::Remind(text) => self.deliver_mechanical_text(request, event, text),
        }
    }

    pub(super) fn apply_startup_effect(&self, job: &JobRequest, effect: JobEffect) {
        match effect {
            JobEffect::Quiet => {
                tracing::info!(job = %job.name, "startup snapshot clean");
            }
            JobEffect::RemindRows(items) => {
                tracing::warn!(job = %job.name, count = items.len(), "startup snapshot needs attention");
                let now = chrono::Utc::now();
                self.send_reminder_rows(&items, job, false, now);
            }
            JobEffect::Continue => {}
            JobEffect::Remind(text) => {
                tracing::warn!(job = %job.name, "startup snapshot needs attention");
                let now = chrono::Utc::now();
                self.send_reminder(&text, job, false, now);
            }
        }
    }

    fn deliver_mechanical_rows(
        &self,
        request: &JobRequest,
        event: &Event,
        items: Vec<crate::reminders::MechanicalReminder>,
    ) -> Option<ReactionOutcome> {
        tracing::info!(job = %request.name, kind = %request.kind, count = items.len(), "mechanical job reminder rows");
        let fire_at = crate::helpers::cron_fire_at(event);
        let catch_up = crate::helpers::cron_catch_up(&event.payload.data);
        self.send_reminder_rows(&items, request, catch_up, fire_at);
        Some(ReactionOutcome::Observed)
    }

    fn deliver_mechanical_text(
        &self,
        request: &JobRequest,
        event: &Event,
        text: String,
    ) -> Option<ReactionOutcome> {
        tracing::info!(job = %request.name, kind = %request.kind, "mechanical job reminder");
        let fire_at = crate::helpers::cron_fire_at(event);
        let catch_up = crate::helpers::cron_catch_up(&event.payload.data);
        self.send_reminder(&text, request, catch_up, fire_at);
        Some(ReactionOutcome::Observed)
    }
}
