//! Boot wait and later `chat: tool surface ready` lines.
//!
//! Homelab reads `chat: tool surface ready count=N` and expects the full face
//! surface (MCP tools plus the non-MCP tools the face lists). Boot waits one
//! bounded interval so that line can include peers that are already up. Later
//! connect and drop events log that same count.

use std::sync::Arc;
use std::time::Duration;

use liberado_common::CapabilitySet;
use liberado_executor::ToolRuntime;

/// Upper bound for the first connect wave. Peers still in `connect` stay in the background.
pub(crate) const FIRST_WAVE_BOUND: Duration = Duration::from_secs(15);

pub(crate) async fn watch_after_first_wave(
    live: &liberado_mcp::LiveRegistryRuntime,
    runtime: Arc<dyn ToolRuntime>,
    delegation_mode: bool,
    caps: CapabilitySet,
    workspace_offered: bool,
) {
    live.wait_first_wave(FIRST_WAVE_BOUND).await;
    live.arm_surface_hook(surface_hook(
        runtime,
        delegation_mode,
        caps,
        workspace_offered,
    ));
}

fn surface_hook(
    runtime: Arc<dyn ToolRuntime>,
    delegation_mode: bool,
    caps: CapabilitySet,
    workspace_offered: bool,
) -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(move || {
        let _ = super::face_tool_surface(&runtime, delegation_mode, &caps, workspace_offered);
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use liberado_common::{Capability, CapabilitySet, WriteProvenance};
    use liberado_executor::{RuntimeSetupError, ToolRuntime};
    use liberado_mcp::{
        LiveRegistryRuntime, McpConnector, McpRegistry, RebindableRuntime, ReconnectPolicy,
    };
    use liberado_provider::{ToolDef, ToolInvocation};
    use tracing::Subscriber;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};

    use super::FIRST_WAVE_BOUND;

    #[test]
    fn first_wave_bound_is_fifteen_seconds() {
        assert_eq!(FIRST_WAVE_BOUND, std::time::Duration::from_secs(15));
    }

    #[tokio::test(start_paused = true, flavor = "current_thread")]
    async fn ready_lines_count_the_face_surface() {
        let counts = Arc::new(Mutex::new(Vec::<String>::new()));
        let local = tokio::task::LocalSet::new();
        local
            .run_until(collect_ready_lines(Arc::clone(&counts)))
            .await;
        let got = counts.lock().unwrap_or_else(|err| err.into_inner()).clone();
        let ready = ready_counts(&got);
        assert_eq!(ready, vec![3, 2, 3], "{got:?}");
    }

    async fn collect_ready_lines(counts: Arc<Mutex<Vec<String>>>) {
        let _guard = tracing::subscriber::set_default(ReadyCounts(Arc::clone(&counts)));
        let gate = Arc::new(AtomicBool::new(false));
        let live = Arc::new(LiveRegistryRuntime::with_policy(
            McpRegistry::new().register(
                "memory",
                GatedConnector {
                    gate: Arc::clone(&gate),
                },
            ),
            WriteProvenance::agent("test", "c"),
            ReconnectPolicy::exact(
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(60),
                std::time::Duration::from_secs(3600),
            ),
        ));
        let runtime: Arc<dyn ToolRuntime> = live.clone();
        let caps = memory_caps();
        super::watch_after_first_wave(&live, Arc::clone(&runtime), true, caps.clone(), false).await;
        let (names, count) = super::super::face_tool_surface(&runtime, true, &caps, false);
        assert_eq!(count, 3, "{names:?}");
        assert!(names.iter().any(|name| name == "memory:search"));
        assert!(names.iter().any(|name| name == "delegate"));
        assert!(names.iter().any(|name| name == "create_agent"));

        gate.store(true, Ordering::SeqCst);
        let err = runtime
            .invoke(&ToolInvocation::new(
                "c",
                "memory:search",
                serde_json::json!({}),
            ))
            .await
            .unwrap_err();
        assert!(err.contains("unavailable"), "{err}");
        spin_until("catalog empty after death", || runtime.catalog().is_empty()).await;

        gate.store(false, Ordering::SeqCst);
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        spin_until("tools back after reconnect", || {
            runtime.catalog().len() == 1
        })
        .await;
        let got = counts.lock().unwrap_or_else(|err| err.into_inner()).clone();
        let ready = ready_counts(&got);
        assert_eq!(ready, vec![3, 2, 3], "{got:?}");
    }

    fn ready_counts(rows: &[String]) -> Vec<u64> {
        rows.iter()
            .filter_map(|row| {
                row.strip_prefix("chat: tool surface ready count=")?
                    .parse()
                    .ok()
            })
            .collect()
    }

    fn memory_caps() -> CapabilitySet {
        let mut caps = CapabilitySet::empty();
        caps.grant(Capability::ExecuteMcp("memory".into()));
        caps
    }

    async fn spin_until(label: &str, mut ready: impl FnMut() -> bool) {
        for _ in 0..400 {
            if ready() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("{label}");
    }

    struct GatedConnector {
        gate: Arc<AtomicBool>,
    }

    struct GatedTool {
        gate: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl McpConnector for GatedConnector {
        async fn connect(
            &self,
            _: WriteProvenance,
        ) -> Result<Box<dyn RebindableRuntime>, RuntimeSetupError> {
            Ok(Box::new(GatedTool {
                gate: Arc::clone(&self.gate),
            }))
        }
    }

    #[async_trait::async_trait]
    impl ToolRuntime for GatedTool {
        fn catalog(&self) -> Vec<ToolDef> {
            vec![ToolDef {
                name: "search".into(),
                description: String::new(),
                parameters: serde_json::json!({}),
            }]
        }

        async fn invoke(&self, _: &ToolInvocation) -> Result<String, String> {
            if self.gate.load(Ordering::SeqCst) {
                Err("peer down".into())
            } else {
                Ok("ok".into())
            }
        }
    }

    #[async_trait::async_trait]
    impl RebindableRuntime for GatedTool {
        fn rebind_provenance(&mut self, _: WriteProvenance) {}

        fn connection_is_dead(&self) -> bool {
            self.gate.load(Ordering::SeqCst)
        }
    }

    struct ReadyCounts(Arc<Mutex<Vec<String>>>);

    impl Subscriber for ReadyCounts {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _: &Id, _: &Record<'_>) {}

        fn record_follows_from(&self, _: &Id, _: &Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            let mut take = TakeCount::default();
            event.record(&mut take);
            if take.message.as_deref() == Some("chat: tool surface ready") {
                let Some(count) = take.count else {
                    return;
                };
                self.0
                    .lock()
                    .unwrap_or_else(|err| err.into_inner())
                    .push(format!("chat: tool surface ready count={count}"));
            }
        }

        fn enter(&self, _: &Id) {}

        fn exit(&self, _: &Id) {}
    }

    #[derive(Default)]
    struct TakeCount {
        count: Option<u64>,
        message: Option<String>,
    }

    impl Visit for TakeCount {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            let text = format!("{value:?}");
            let text = text.trim_matches('"');
            if field.name() == "message" {
                self.message = Some(text.to_string());
            }
            if field.name() == "count" && self.count.is_none() {
                self.count = text.parse().ok();
            }
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            if field.name() == "message" {
                self.message = Some(value.to_string());
            }
        }

        fn record_u64(&mut self, field: &Field, value: u64) {
            if field.name() == "count" {
                self.count = Some(value);
            }
        }

        fn record_i64(&mut self, field: &Field, value: i64) {
            if field.name() == "count" {
                self.count = u64::try_from(value).ok();
            }
        }
    }
}
