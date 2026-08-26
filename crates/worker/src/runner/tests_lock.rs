//! Concurrency tests for the per-repository clone lock (live finding 2026-08-26).

use std::sync::Arc;

/// Two sections over the same repository must not interleave: a fetch racing a
/// fetch is what killed a live task with "cannot lock ref refs/remotes/origin/main".
#[tokio::test]
async fn repo_lock_serializes_sections_for_the_same_repository() {
    use std::sync::Mutex as StdMutex;
    let order: Arc<StdMutex<Vec<&'static str>>> = Arc::default();
    let one = crate::runner::with_repo_lock("repo-a", {
        let order = order.clone();
        async move || {
            order.lock().unwrap().push("a-start");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            order.lock().unwrap().push("a-end");
            Ok(())
        }
    });
    let two = crate::runner::with_repo_lock("repo-a", {
        let order = order.clone();
        async move || {
            order.lock().unwrap().push("b-start");
            order.lock().unwrap().push("b-end");
            Ok(())
        }
    });
    let (r1, r2) = tokio::join!(one, two);
    r1.expect("one");
    r2.expect("two");

    let order = order.lock().unwrap().clone();
    assert!(
        order == vec!["a-start", "a-end", "b-start", "b-end"]
            || order == vec!["b-start", "b-end", "a-start", "a-end"],
        "sections interleaved: {order:?}"
    );
}

#[tokio::test]
async fn different_repositories_do_not_serialize_each_other() {
    let release = Arc::new(tokio::sync::Notify::new());
    let holder = crate::runner::with_repo_lock("slow-repo", {
        let release = release.clone();
        async move || {
            release.notified().await;
            Ok(())
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    // A different repo must proceed while slow-repo holds its section.
    crate::runner::with_repo_lock("other-repo", async || Ok::<(), String>(()))
        .await
        .expect("independent repos do not block each other");
    release.notify_one();
    holder.await.expect("holder completes");
}
