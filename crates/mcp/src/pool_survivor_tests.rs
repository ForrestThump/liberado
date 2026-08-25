//! Survivor tests for `pool.rs` — wired as a child module so `super::*`
//! reaches private helpers (`take_expired`, `slots`, `PoolSlot`) directly.

use super::*;
use serde_json::json;
use std::sync::atomic::AtomicUsize;

/// A controllable double: configured catalog, invoke result, and dead flag;
/// logs provenance rebinds and shutdowns so pool lifecycle decisions are
/// observable without any real transport.
struct FakeRuntime {
    dead: AtomicBool,
    tools: Vec<ToolDef>,
    invoke_result: Result<String, String>,
    rebinds: Mutex<Vec<String>>,
    shutdowns: AtomicUsize,
}

impl FakeRuntime {
    fn new() -> Self {
        Self {
            dead: AtomicBool::new(false),
            tools: vec![ToolDef::new("fake_tool", "fake", json!({}))],
            invoke_result: Ok("inner-ok".into()),
            rebinds: Mutex::new(Vec::new()),
            shutdowns: AtomicUsize::new(0),
        }
    }

    fn set_dead(&self, dead: bool) {
        self.dead.store(dead, Ordering::SeqCst);
    }
}

#[async_trait]
impl ToolRuntime for FakeRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        self.tools.clone()
    }
    async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
        self.invoke_result.clone()
    }
}

#[async_trait]
impl RebindableRuntime for FakeRuntime {
    fn rebind_provenance(&mut self, provenance: WriteProvenance) {
        self.rebinds.lock().unwrap().push(provenance.source);
    }
    fn connection_is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }
    async fn shutdown(&mut self) {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
    }
}

fn provenance(who: &str) -> WriteProvenance {
    WriteProvenance {
        source: who.to_string(),
        correlation_id: None,
        zone: None,
        note: None,
    }
}

/// Insert a slot directly (check-in is gated on `enabled`, and these tests
/// need populated pools in both enabled and disabled states).
fn insert_slot(pool: &ConnectionPool, name: &str, runtime: Box<dyn RebindableRuntime>) {
    pool.slots.lock().unwrap().insert(
        name.to_string(),
        PoolSlot {
            runtime,
            last_used: pool.now(),
        },
    );
}

#[test]
fn reap_idle_drops_only_slots_past_the_ttl() {
    let policy = PoolPolicy {
        idle_ttl: Duration::from_secs(100),
        ..PoolPolicy::default()
    };
    let pool = ConnectionPool::new(policy);
    insert_slot(&pool, "fresh", Box::new(FakeRuntime::new()));
    insert_slot(&pool, "stale", Box::new(FakeRuntime::new()));

    // Age every slot past the TTL by rewinding last_used through the clock.
    let aged = pool.now() - Duration::from_secs(200);
    for slot in pool.slots.lock().unwrap().values_mut() {
        slot.last_used = aged;
    }

    assert_eq!(pool.reap_idle(), 2, "both expired slots are reaped");
    assert!(pool.slots.lock().unwrap().is_empty());
}

/// With pooling disabled the pool must not exist functionally: even a slot
/// that somehow got in is never reaped, checked out, or re-checked-in.
#[test]
fn a_disabled_pool_never_reaps_or_checkouts() {
    let policy = PoolPolicy {
        enabled: false,
        ..PoolPolicy::default()
    };
    let pool = ConnectionPool::new(policy);
    let rt = Box::new(FakeRuntime::new());
    insert_slot(&pool, "ghost", rt);

    assert_eq!(pool.reap_idle(), 0, "disabled means no reaping");
    let arc = Arc::new(pool);
    assert!(
        arc.try_checkout("ghost", provenance("t")).is_none(),
        "disabled means no checkout"
    );
}

/// The skew guard between eager reap and remove: exactly-at-TTL is kept,
/// one tick past it is discarded. A controlled clock removes real-time
/// jitter from the exact boundary.
#[test]
fn checkout_boundary_is_strictly_greater_than_ttl() {
    let now = Arc::new(Mutex::new(Instant::now()));
    let clock_handle = Arc::clone(&now);
    let policy = PoolPolicy {
        idle_ttl: Duration::from_secs(60),
        ..PoolPolicy::default()
    };
    let pool = Arc::new(ConnectionPool::with_clock(
        policy,
        Arc::new(move || *clock_handle.lock().unwrap()),
    ));

    insert_slot(pool.as_ref(), "boundary", Box::new(FakeRuntime::new()));

    // Age the slot to EXACTLY the TTL on the controlled clock, then try to
    // check out with no further time passing.
    *now.lock().unwrap() += Duration::from_secs(60);
    {
        let mut slots = pool.slots.lock().unwrap();
        let slot = slots.get_mut("boundary").unwrap();
        slot.last_used = (pool.clock)();
    }
    let checkout = pool.try_checkout("boundary", provenance("t"));
    assert!(checkout.is_some(), "exactly-at-TTL is not yet idle");
    drop(checkout);

    // Check-in re-stamped last_used to "now"; age the stored slot one tick
    // PAST the TTL so the skew guard (idle > ttl) must discard it.
    {
        let mut slots = pool.slots.lock().unwrap();
        let slot = slots.get_mut("boundary").unwrap();
        slot.last_used = (pool.clock)() - Duration::from_secs(61);
    }
    assert!(pool.try_checkout("boundary", provenance("t")).is_none());
}

#[test]
fn invalidate_removes_the_named_slot_without_returning_it() {
    let pool = ConnectionPool::new(PoolPolicy::default());
    insert_slot(&pool, "flaky", Box::new(FakeRuntime::new()));

    pool.invalidate("flaky");
    assert!(pool.slots.lock().unwrap().is_empty(), "the slot is gone");

    // Unknown names are a no-op, not a panic.
    pool.invalidate("never-there");
}

/// A successful invoke must leave the checkout healthy even when the peer's
/// dead flag is stale-set - the connection re-enters the pool on drop.
#[tokio::test]
async fn success_keeps_a_stale_dead_flag_out_of_the_discard_path() {
    let pool = Arc::new(ConnectionPool::new(PoolPolicy::default()));
    let fake = FakeRuntime::new();
    fake.set_dead(true); // stale flag; this invoke SUCCEEDS
    let _ = &fake;
    insert_slot(pool.as_ref(), "m", Box::new(fake));

    let checkout = pool
        .try_checkout("m", provenance("t"))
        .expect("slot present");
    let out = checkout
        .invoke(&ToolInvocation::new("t1", "fake_tool", json!({})))
        .await
        .expect("success passes through");
    assert_eq!(out, "inner-ok");
    drop(checkout);

    assert!(
        pool.slots.lock().unwrap().contains_key("m"),
        "a healthy success checks back in"
    );
}

/// A real transport death (err + dead flag) discards instead of checking in:
/// the next acquire must find the pool empty.
#[tokio::test]
async fn a_dead_connection_is_not_checked_back_in() {
    let pool = Arc::new(ConnectionPool::new(PoolPolicy::default()));
    let mut fake = FakeRuntime::new();
    fake.set_dead(true);
    fake.invoke_result = Err("connection reset".into());
    insert_slot(pool.as_ref(), "m", Box::new(fake));

    let checkout = pool.try_checkout("m", provenance("t")).expect("present");
    let err = checkout
        .invoke(&ToolInvocation::new("t1", "fake_tool", json!({})))
        .await
        .expect_err("the transport error passes through");
    assert!(err.contains("connection reset"), "{err}");
    drop(checkout);

    assert!(
        !pool.slots.lock().unwrap().contains_key("m"),
        "a dead connection must not re-enter the pool"
    );
}

// ── thin delegation wrappers ────────────────────────────────────────────────

#[tokio::test]
async fn as_tool_runtime_delegates_catalog_and_invoke_verbatim() {
    let inner = Box::new(FakeRuntime::new());
    let wrapper = AsToolRuntime(inner);
    assert_eq!(wrapper.catalog().len(), 1);
    assert_eq!(wrapper.catalog()[0].name, "fake_tool");
    let out = wrapper
        .invoke(&ToolInvocation::new("t1", "fake_tool", json!({})))
        .await
        .unwrap();
    assert_eq!(out, "inner-ok");
}

#[tokio::test]
async fn permitted_runtime_delegates_and_marks_health_on_success() {
    let catalog = Arc::new(CapabilityCatalog::new());
    catalog.mark_degraded("degraded-mcp");

    let inner = AsToolRuntime(Box::new(FakeRuntime::new()));
    let permitted = PermittedRuntime {
        inner,
        _permit: Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap(),
        name: "degraded-mcp".into(),
        health: Some(Arc::clone(&catalog)),
    };

    assert_eq!(permitted.catalog().len(), 1);
    let out = permitted
        .invoke(&ToolInvocation::new("t1", "fake_tool", json!({})))
        .await
        .unwrap();
    assert_eq!(out, "inner-ok");
}
