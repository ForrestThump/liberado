use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::batch::{DeliveryBatch, DeliveryOutcome};
use crate::error::Result;
use crate::plan::coalesce;
use crate::store::Outbox;

#[derive(Debug, Clone, Copy)]
pub struct PassLimits {
    pub max_catchup: u32,
    pub max_attempts: i64,
}

impl Default for PassLimits {
    fn default() -> Self {
        Self {
            max_catchup: 20,
            max_attempts: 8,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PassReport {
    pub accepted: u32,
    pub failed: u32,
    pub held: u32,
}

#[async_trait]
pub trait DeliverySink: Send + Sync {
    fn readiness(&self, channel: &str, target: &str) -> std::result::Result<(), String>;
    async fn deliver(&self, batch: &DeliveryBatch) -> DeliveryOutcome;
}

pub async fn run_pass(
    outbox: &Outbox,
    sink: &dyn DeliverySink,
    now: DateTime<Utc>,
    limits: PassLimits,
) -> Result<PassReport> {
    outbox.prune(now)?;
    let due = outbox.due_rows(now, limits.max_catchup)?;
    let batches = coalesce(due, now);
    let mut report = PassReport::default();
    for batch in batches {
        apply_one(outbox, sink, &batch, now, limits, &mut report).await?;
    }
    Ok(report)
}

async fn apply_one(
    outbox: &Outbox,
    sink: &dyn DeliverySink,
    batch: &DeliveryBatch,
    now: DateTime<Utc>,
    limits: PassLimits,
    report: &mut PassReport,
) -> Result<()> {
    let ids = batch.ids();
    if let Err(message) = sink.readiness(&batch.channel, &batch.target) {
        outbox.note(&ids, &message)?;
        report.held += 1;
        return Ok(());
    }
    outbox.lease(&ids, now)?;
    match sink.deliver(batch).await {
        DeliveryOutcome::Accepted => {
            outbox.mark_accepted(&ids, now)?;
            report.accepted += 1;
        }
        DeliveryOutcome::Gone => {
            if let Some(id) = sub_id(&batch.target) {
                outbox.delete_subscription(id)?;
            }
            outbox.mark_dead(&ids, "subscription_gone", now)?;
            report.failed += 1;
        }
        DeliveryOutcome::Retryable(message) => {
            outbox.mark_retry(&ids, &message, now, limits.max_attempts)?;
            report.failed += 1;
        }
        DeliveryOutcome::Unconfigured(message) => {
            outbox.release(&ids, &message)?;
            report.held += 1;
        }
    }
    Ok(())
}

fn sub_id(target: &str) -> Option<i64> {
    target.strip_prefix("sub:")?.parse().ok()
}
