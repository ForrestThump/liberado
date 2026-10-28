//! One reconnect task per MCP peer.
//!
//! Split from `supervisor.rs` so the state machine and the task loop stay under the
//! module-health function ceiling.

use std::sync::Arc;
use std::time::Duration;

use liberado_tool_runtime::{RebindableRuntime, RuntimeSetupError};

use super::{ConnectOnce, DropWhy, Latch, PeerCtx, PeerSupervisor, Pending, Step};
use crate::backoff::{self, FailureDecision, FailureLog};

pub(super) fn spawn_peer(sup: &Arc<PeerSupervisor>, generation: u64, item: Pending) {
    let ctx = PeerCtx {
        name: item.name,
        generation,
        connector: item.connector,
        provenance: sup.provenance.clone(),
        policy: sup.policy.clone(),
        cancel: item.cancel,
        sup: Arc::clone(sup),
    };
    tokio::spawn(run_peer(ctx));
}

pub(super) async fn run_control(
    sup: Arc<PeerSupervisor>,
    mut rx: tokio::sync::watch::Receiver<u64>,
) {
    loop {
        let generation = *rx.borrow_and_update();
        let Some(connectors) = sup.connectors() else {
            sup.stop_all();
            break;
        };
        sup.reconcile(generation, connectors);
        if rx.changed().await.is_err() {
            sup.stop_all();
            break;
        }
    }
}

async fn run_peer(ctx: PeerCtx) {
    let mut cursor = crate::backoff::BackoffCursor::default();
    while !ctx.cancel.is_tripped() {
        if let Step::Stop = step_peer(&ctx, &mut cursor).await {
            break;
        }
    }
    ctx.sup.drop_if_owner(&ctx);
    ctx.sup.log_surface_if_changed();
}

async fn step_peer(ctx: &PeerCtx, cursor: &mut crate::backoff::BackoffCursor) -> Step {
    match connect_once(ctx).await {
        ConnectOnce::Cancelled => Step::Stop,
        ConnectOnce::Failed(err) => backoff(ctx, cursor, &err).await,
        ConnectOnce::Ready(runtime) => hold(ctx, cursor, runtime).await,
    }
}

async fn connect_once(ctx: &PeerCtx) -> ConnectOnce {
    let connect = ctx.connector.connect(ctx.provenance.clone());
    tokio::pin!(connect);
    tokio::select! {
        biased;
        _ = ctx.cancel.wait() => ConnectOnce::Cancelled,
        result = &mut connect => classify_connect(result),
    }
}

fn classify_connect(result: Result<Box<dyn RebindableRuntime>, RuntimeSetupError>) -> ConnectOnce {
    match result {
        Ok(runtime) => ConnectOnce::Ready(runtime),
        Err(err) => ConnectOnce::Failed(err.to_string()),
    }
}

async fn backoff(ctx: &PeerCtx, cursor: &mut crate::backoff::BackoffCursor, err: &str) -> Step {
    let decision = cursor.record(ctx.policy.base, ctx.policy.cap, ctx.policy.unit());
    let delay = backoff::floor_delay(decision.delay);
    ctx.sup.mark_degraded(ctx, decision.attempt, err, delay);
    publish_health(&ctx.sup, &ctx.name, false);
    emit_failure_log(&ctx.name, err, &decision);
    ctx.sup.log_surface_if_changed();
    wait_delay(&ctx.cancel, delay).await
}

async fn wait_delay(cancel: &Latch, delay: Duration) -> Step {
    tokio::select! {
        biased;
        _ = cancel.wait() => Step::Stop,
        _ = tokio::time::sleep(delay) => Step::Again,
    }
}

async fn hold(
    ctx: &PeerCtx,
    cursor: &mut crate::backoff::BackoffCursor,
    runtime: Box<dyn RebindableRuntime>,
) -> Step {
    let attempts = cursor.success_attempts();
    cursor.reset();
    let runtime: Arc<dyn RebindableRuntime> = Arc::from(runtime);
    let tools = runtime.catalog().len();
    let death = Arc::new(Latch::new());
    if !ctx
        .sup
        .install(ctx, Arc::clone(&runtime), Arc::clone(&death), tools)
    {
        release_runtime(&runtime).await;
        return Step::Stop;
    }
    log_reconnected(&ctx.name, tools, attempts);
    ctx.sup.log_surface_if_changed();
    publish_health(&ctx.sup, &ctx.name, true);
    let why = watch_until_drop(ctx, &runtime, &death).await;
    ctx.sup.uninstall(ctx);
    release_runtime(&runtime).await;
    ctx.sup.log_surface_if_changed();
    finish_drop(ctx, cursor, why).await
}

async fn finish_drop(
    ctx: &PeerCtx,
    cursor: &mut crate::backoff::BackoffCursor,
    why: DropWhy,
) -> Step {
    let DropWhy::Failed(err) = why else {
        return Step::Stop;
    };
    if ctx.cancel.is_tripped() {
        return Step::Stop;
    }
    backoff(ctx, cursor, &err).await
}

async fn watch_until_drop(
    ctx: &PeerCtx,
    runtime: &Arc<dyn RebindableRuntime>,
    death: &Latch,
) -> DropWhy {
    let mut probe = tokio::time::interval(probe_period(ctx.policy.probe_every));
    probe.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first interval tick is immediate. Skip it so a paused test does not flap.
    probe.tick().await;
    loop {
        tokio::select! {
            biased;
            _ = ctx.cancel.wait() => return DropWhy::Removed,
            _ = death.wait() => return DropWhy::Failed("transport error".into()),
            _ = probe.tick() => {
                if let Some(why) = probe_failed(runtime).await {
                    return why;
                }
            }
        }
    }
}

async fn probe_failed(runtime: &Arc<dyn RebindableRuntime>) -> Option<DropWhy> {
    match runtime.probe().await {
        Ok(()) => None,
        Err(err) => Some(DropWhy::Failed(err)),
    }
}

fn probe_period(every: Duration) -> Duration {
    if every < Duration::from_millis(1) {
        Duration::from_millis(1)
    } else {
        every
    }
}

async fn release_runtime(runtime: &Arc<dyn RebindableRuntime>) {
    runtime.shutdown_shared().await;
}

fn publish_health(sup: &PeerSupervisor, name: &str, up: bool) {
    let Some(inner) = sup.registry.upgrade() else {
        return;
    };
    let guard = inner.health.read().unwrap_or_else(|err| err.into_inner());
    let Some(catalog) = guard.as_ref() else {
        return;
    };
    if up {
        catalog.mark_healthy(name);
    } else {
        catalog.mark_degraded(name);
    }
}

fn emit_failure_log(name: &str, err: &str, decision: &FailureDecision) {
    match decision.kind {
        FailureLog::First => log_first_failure(name, err),
        FailureLog::StillDown => log_still_down(name, err, decision.attempt),
        FailureLog::Silent => {}
    }
}

fn log_first_failure(name: &str, err: &str) {
    tracing::warn!(mcp = %name, error = %err, "MCP failed to connect — continuing without it");
}

fn log_still_down(name: &str, err: &str, attempts: u32) {
    tracing::warn!(
        mcp = %name,
        error = %err,
        attempts,
        "MCP failed to connect — continuing without it; still down"
    );
}

fn log_reconnected(name: &str, tools: usize, attempts: u32) {
    tracing::info!(mcp = %name, tools, attempts, "MCP reconnected");
}
