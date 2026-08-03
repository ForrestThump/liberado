//! One module per path (P1a–P7).

mod p1a;
mod p1b;
mod p2;
mod p3;
mod p4;
mod p5;
mod p6;
pub mod p7;

use std::time::Instant;

use crate::client::DaemonClient;
use crate::config::ConformanceConfig;
use crate::result::{PathId, PathResult};

pub async fn run_path(
    id: PathId,
    client: &DaemonClient,
    cfg: &ConformanceConfig,
    deadline: Instant,
) -> PathResult {
    if Instant::now() >= deadline {
        return PathResult::fail(
            id,
            "budget",
            0,
            serde_json::json!({"reason": "budget_exhausted_before_path"}),
        );
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    let timeout = cfg.path_timeout().min(remaining);

    let start = Instant::now();
    let result = match id {
        PathId::P1a => p1a::run(client, cfg, timeout).await,
        PathId::P1b => p1b::run(client, cfg, timeout).await,
        PathId::P2 => p2::run(client, cfg, timeout).await,
        PathId::P3 => p3::run(client, cfg, timeout).await,
        PathId::P4 => p4::run(client, cfg, timeout).await,
        PathId::P5 => p5::run(client, cfg, timeout).await,
        PathId::P6 => p6::run(client, cfg, timeout).await,
        PathId::P7 => p7::run(client, cfg, timeout).await,
    };
    let _ = start;
    result
}

pub(crate) fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}
