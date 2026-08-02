//! The `ConversationStore` conformance suite — run against the store **production actually uses**.
//!
//! These tests used to live in `liberado-conversation-store` and exercise its `JsonlStore`. After
//! the D7 store convergence, production chat runs on `liberado-session-store::SessionStore` and
//! nothing outside tests constructed a `JsonlStore` at all — so a suite of fourteen load-bearing
//! storage invariants was quietly guarding an implementation no user could reach, while the one
//! doing the real work went unverified.
//!
//! That was not theoretical. Moving the suite here immediately caught two live defects in
//! `SessionStore` that no chat test could have found: ids minted non-monotonically, and a durable
//! append that wrote its line *outside* the lock it minted under (so file order could disagree with
//! id order, and two concurrent appends could interleave mid-line and corrupt the log).
//!
//! Covered: schema round-trips, DAG traversal, the file-order == id-order invariant, durability
//! across store instances, and concurrent appends.

use std::sync::Arc;
use std::time::Duration;

use liberado_conversation_store::{
    Author, ConversationStore, NewConversation, NewNode, StoreError, Ulid,
};
use liberado_provider::{Message, ToolInvocation};
use liberado_session_store::SessionStore;
use tempfile::tempdir;

/// A durable store rooted at `dir` — the same call `liberado-server` makes at boot.
async fn store_at(dir: &std::path::Path) -> SessionStore {
    SessionStore::open(dir).await
}

/// A conversation with no lineage and the given title.
fn new_convo(title: &str) -> NewConversation {
    NewConversation {
        title: Some(title.to_string()),
        parent_conversation: None,
        spawned_by: None,
        ephemeral: false,
        visibility: Default::default(),
        grant: Default::default(),
    }
}

/// A user node replying to `parent`.
fn user_node(parent: Option<ulid::Ulid>, content: &str) -> NewNode {
    NewNode {
        parent_id: parent,
        author: Author::User,
        message: Message::user(content),
        model: None,
    }
}

#[tokio::test]
async fn create_then_list_returns_the_header() {
    let dir = tempdir().unwrap();
    let store = store_at(dir.path()).await;

    let header = store.create(new_convo("first chat")).await.unwrap();

    let listed = store.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, header.id);
    assert_eq!(listed[0].title.as_deref(), Some("first chat"));
}

#[tokio::test]
async fn linear_append_yields_ordered_increasing_path() {
    let dir = tempdir().unwrap();
    let store = store_at(dir.path()).await;
    let convo = store.create(new_convo("linear")).await.unwrap().id;

    let n0 = store.append(convo, user_node(None, "one")).await.unwrap();
    let n1 = store
        .append(convo, user_node(Some(n0.id), "two"))
        .await
        .unwrap();
    let n2 = store
        .append(convo, user_node(Some(n1.id), "three"))
        .await
        .unwrap();

    let path = store.leaf_path(convo, None).await.unwrap();
    let contents: Vec<&str> = path.iter().map(|n| n.message.content.as_str()).collect();
    assert_eq!(contents, ["one", "two", "three"]);

    // The path is exactly n0 -> n1 -> n2.
    assert_eq!(
        path.iter().map(|n| n.id).collect::<Vec<_>>(),
        [n0.id, n1.id, n2.id]
    );

    // Ids are strictly increasing across the returned path (file-order == id-order).
    assert!(path.windows(2).all(|w| w[0].id < w[1].id));
}

#[tokio::test]
async fn file_order_equals_id_order_after_appends() {
    let dir = tempdir().unwrap();
    let store = store_at(dir.path()).await;
    let convo = store.create(new_convo("ordered")).await.unwrap().id;

    let mut parent = None;
    for i in 0..20 {
        let n = store
            .append(convo, user_node(parent, &format!("m{i}")))
            .await
            .unwrap();
        parent = Some(n.id);
    }

    let path = store.leaf_path(convo, None).await.unwrap();
    assert_eq!(path.len(), 20);
    assert!(
        path.windows(2).all(|w| w[0].id < w[1].id),
        "ids must be strictly ascending in leaf-path (file) order"
    );
}

#[tokio::test]
async fn branching_splits_and_rejoins_correctly() {
    let dir = tempdir().unwrap();
    let store = store_at(dir.path()).await;
    let convo = store.create(new_convo("branchy")).await.unwrap().id;

    let a = store.append(convo, user_node(None, "A")).await.unwrap();
    let b = store
        .append(convo, user_node(Some(a.id), "B"))
        .await
        .unwrap();
    let c = store
        .append(convo, user_node(Some(a.id), "C"))
        .await
        .unwrap();

    // A has both B and C as children.
    let mut expected = vec![b.id, c.id];
    expected.sort();
    assert_eq!(store.children(convo, a.id).await.unwrap(), expected);

    // Each branch is its own distinct path A -> {B,C}.
    let path_b = store.leaf_path(convo, Some(b.id)).await.unwrap();
    assert_eq!(
        path_b.iter().map(|n| n.id).collect::<Vec<_>>(),
        [a.id, b.id]
    );
    let path_c = store.leaf_path(convo, Some(c.id)).await.unwrap();
    assert_eq!(
        path_c.iter().map(|n| n.id).collect::<Vec<_>>(),
        [a.id, c.id]
    );

    // `None` follows the greatest id, which is C (appended last).
    let path_default = store.leaf_path(convo, None).await.unwrap();
    assert_eq!(path_default.last().unwrap().id, c.id);
}

#[tokio::test]
async fn node_lookup_hit_and_miss() {
    let dir = tempdir().unwrap();
    let store = store_at(dir.path()).await;
    let convo = store.create(new_convo("lookup")).await.unwrap().id;

    let n = store.append(convo, user_node(None, "here")).await.unwrap();

    let found = store.node(convo, n.id).await.unwrap();
    assert_eq!(found.as_ref(), Some(&n));

    let missing = store.node(convo, ulid::Ulid::new()).await.unwrap();
    assert_eq!(missing, None);
}

#[tokio::test]
async fn missing_conversation_is_not_found() {
    let dir = tempdir().unwrap();
    let store = store_at(dir.path()).await;
    let ghost = ulid::Ulid::new();

    assert!(matches!(
        store.leaf_path(ghost, None).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        store.append(ghost, user_node(None, "x")).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        store.node(ghost, ulid::Ulid::new()).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn tool_call_messages_round_trip() {
    let dir = tempdir().unwrap();
    let store = store_at(dir.path()).await;
    let convo = store.create(new_convo("tools")).await.unwrap().id;

    // An assistant node carrying a tool_calls invocation.
    let mut assistant = Message::assistant("calling a tool");
    assistant.tool_calls = vec![ToolInvocation::new(
        "call_1",
        "search",
        serde_json::json!({ "q": "rust" }),
    )];
    let asst_node = store
        .append(
            convo,
            NewNode {
                parent_id: None,
                author: Author::Assistant,
                message: assistant,
                model: None,
            },
        )
        .await
        .unwrap();

    // A tool node answering it via tool_call_id.
    let tool_node = store
        .append(
            convo,
            NewNode {
                parent_id: Some(asst_node.id),
                author: Author::Tool,
                message: Message::tool_result("call_1", "{\"hits\":3}"),
                model: None,
            },
        )
        .await
        .unwrap();

    // Reload from disk and assert exact PartialEq equality with what was appended.
    let reloaded_asst = store.node(convo, asst_node.id).await.unwrap().unwrap();
    assert_eq!(reloaded_asst, asst_node);
    assert_eq!(reloaded_asst.message.tool_calls[0].name.as_str(), "search");

    let reloaded_tool = store.node(convo, tool_node.id).await.unwrap().unwrap();
    assert_eq!(reloaded_tool, tool_node);
    assert_eq!(
        reloaded_tool.message.tool_call_id.as_deref(),
        Some("call_1")
    );
}

#[tokio::test]
async fn data_survives_across_store_instances() {
    let dir = tempdir().unwrap();

    let (convo, appended) = {
        let store = store_at(dir.path()).await;
        let convo = store.create(new_convo("durable")).await.unwrap().id;
        let n = store
            .append(convo, user_node(None, "persist me"))
            .await
            .unwrap();
        (convo, n)
    }; // first store dropped — only disk remains

    // A brand-new instance over the same root must see the on-disk data.
    let reopened = store_at(dir.path()).await;
    let path = reopened.leaf_path(convo, None).await.unwrap();
    assert_eq!(path.len(), 1);
    assert_eq!(path[0], appended);

    let listed = reopened.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, convo);
}

// **Really** parallel, and it took two goes to make it so.
//
// This test existed for months and proved nothing. Two reasons, both easy to miss:
//
//  1. `#[tokio::test]` defaults to a `current_thread` runtime.
//  2. `join_all` polls every future on **one task** — that is concurrency, not parallelism.
//
// `append` mints an id and then writes the line with no `.await` in between, so under either of
// those the critical section can never be preempted, and the appends quietly run one at a time. The
// test passed against a store with no write lock at all.
//
// A multi-threaded runtime **and** `tokio::spawn` (one task each, free to land on different threads)
// is what actually models the daemon — which is multi-threaded, and where a chat turn and a tool
// result really do append to the same session's log at the same time.
//
// Even then, this does not *reliably* fail against a store with no write lock: the race needs the
// scheduler to preempt a task in the window between releasing the in-memory lock and issuing the
// write, and that window is a few instructions wide. It was confirmed real by temporarily inserting
// a `yield_now().await` into that window, which makes it fail every run ("ids must be strictly
// increasing in file order"); putting the write lock back makes it pass every run *with the yield
// still in place*. So the lock is load-bearing, not decorative — but do not expect this test alone
// to catch its removal. The invariant is held by construction; the test is a backstop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_appends_to_one_conversation_are_serialized() {
    let dir = tempdir().unwrap();
    let store = Arc::new(store_at(dir.path()).await);
    let convo = store.create(new_convo("hot")).await.unwrap().id;

    // 50 parallel appends to the SAME conversation (all roots, to keep the test about the writer
    // lock rather than the DAG shape).
    let count = 50;
    let handles: Vec<_> = (0..count)
        .map(|i| {
            let store = store.clone();
            tokio::spawn(async move {
                store
                    .append(convo, user_node(None, &format!("c{i}")))
                    .await
                    .unwrap()
            })
        })
        .collect();
    let mut appended = Vec::with_capacity(count);
    for h in handles {
        appended.push(h.await.expect("append task panicked"));
    }
    assert_eq!(appended.len(), count);

    // Every node landed, with a unique id.
    appended.sort_by_key(|n| n.id);
    appended.dedup_by_key(|n| n.id);
    assert_eq!(appended.len(), count, "ids must be unique across appends");

    // The file is intact: re-reading any leaf parses every node with no corruption. Since all are
    // roots, ask for children of nothing via a full scan: list all nodes by fetching each.
    for n in &appended {
        let got = store.node(convo, n.id).await.unwrap();
        assert!(got.is_some(), "every appended node must be present");
    }

    // Inspect the raw file: every node line parses (no corruption / torn write), and the node ids
    // appear in strictly increasing order — proof that mint-order == write-order under the lock.
    let path = dir.path().join(format!("{convo}.jsonl"));
    let raw = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = raw.split('\n').filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        count + 1,
        "header + one line per node, none lost or torn"
    );

    let mut file_ids = Vec::new();
    for line in lines.iter().skip(1) {
        let value: serde_json::Value = serde_json::from_str(line).expect("every line must parse");
        assert_eq!(value["kind"], "node");
        let id: ulid::Ulid = value["id"].as_str().unwrap().parse().unwrap();
        file_ids.push(id);
    }
    assert!(
        file_ids.windows(2).all(|w| w[0] < w[1]),
        "ids must be strictly increasing in file order"
    );
}

#[tokio::test]
async fn header_returns_the_title_without_walking_the_transcript() {
    let dir = tempdir().unwrap();
    let store = store_at(dir.path()).await;
    let convo = store.create(new_convo("sidebar title")).await.unwrap();
    store
        .append(convo.id, user_node(None, "body"))
        .await
        .unwrap();

    let h = store.header(convo.id).await.unwrap();
    assert_eq!(h.id, convo.id);
    assert_eq!(h.title.as_deref(), Some("sidebar title"));
}

#[tokio::test]
async fn header_missing_conversation_is_not_found() {
    let dir = tempdir().unwrap();
    let store = store_at(dir.path()).await;
    let missing = Ulid::new();
    let err = store.header(missing).await.unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)));
}

#[tokio::test]
async fn set_title_updates_the_header_and_preserves_every_node() {
    let dir = tempdir().unwrap();
    let store = store_at(dir.path()).await;
    let convo = store.create(new_convo("original title")).await.unwrap().id;
    let n0 = store.append(convo, user_node(None, "one")).await.unwrap();
    store
        .append(convo, user_node(Some(n0.id), "two"))
        .await
        .unwrap();

    store
        .set_title(convo, "renamed title".to_string())
        .await
        .unwrap();

    let listed = store.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].title.as_deref(), Some("renamed title"));

    // Both nodes (and their parent-child link) survived the rewrite intact.
    let path = store.leaf_path(convo, None).await.unwrap();
    let contents: Vec<&str> = path.iter().map(|n| n.message.content.as_str()).collect();
    assert_eq!(contents, vec!["one", "two"]);
}

#[tokio::test]
async fn renaming_appends_a_new_header_rather_than_rewriting_the_log() {
    // `JsonlStore` renamed by rewriting the whole file through a temp file + rename. `SessionStore`
    // is strictly append-only — the log's one invariant is that nothing already written is ever
    // mutated — so a rename is simply a *new* header line, and replay takes the last one it sees.
    // That makes the rewrite idempotent and crash-safe for free: a rename interrupted halfway leaves
    // the old header intact rather than a truncated file.
    let dir = tempdir().unwrap();
    let store = store_at(dir.path()).await;
    let convo = store.create(new_convo("title")).await.unwrap().id;
    let n0 = store.append(convo, user_node(None, "one")).await.unwrap();

    store
        .set_title(convo, "new title".to_string())
        .await
        .unwrap();

    let raw = std::fs::read_to_string(dir.path().join(format!("{convo}.jsonl"))).unwrap();
    let headers = raw
        .lines()
        .filter(|l| !l.is_empty())
        .filter(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["kind"] == "header")
        .count();
    assert_eq!(
        headers, 2,
        "a rename appends a header; it never rewrites one"
    );
    assert!(
        !dir.path().join(format!("{convo}.jsonl.tmp")).exists(),
        "append-only means there is no temp file to leave behind"
    );

    // And a reopened store takes the *last* header, with every node still in place.
    let reopened = store_at(dir.path()).await;
    let h = reopened.header(convo).await.unwrap();
    assert_eq!(h.title.as_deref(), Some("new title"));
    assert_eq!(
        reopened.leaf_path(convo, None).await.unwrap()[0].id,
        n0.id,
        "the node survived the rename"
    );
}

#[tokio::test]
async fn create_stores_parent_conversation_lineage() {
    let dir = tempdir().unwrap();
    let store = store_at(dir.path()).await;

    let parent = store.create(new_convo("parent")).await.unwrap();

    let child = store
        .create(NewConversation {
            title: Some("child".into()),
            parent_conversation: Some(parent.id),
            spawned_by: None,
            ephemeral: false,
            visibility: Default::default(),
            grant: Default::default(),
        })
        .await
        .unwrap();

    assert_eq!(child.parent_conversation, Some(parent.id));

    let listed = store.list().await.unwrap();
    assert!(listed.iter().any(|h| h.id == parent.id));
    assert!(listed.iter().any(|h| h.id == child.id));
}

#[tokio::test]
async fn create_stores_spawned_by_lineage() {
    let dir = tempdir().unwrap();
    let store = store_at(dir.path()).await;

    let parent_convo = store.create(new_convo("parent")).await.unwrap();
    let node = store
        .append(parent_convo.id, user_node(None, "spawning message"))
        .await
        .unwrap();

    let spawned = store
        .create(NewConversation {
            title: Some("spawned".into()),
            parent_conversation: None,
            spawned_by: Some(node.id),
            ephemeral: false,
            visibility: Default::default(),
            grant: Default::default(),
        })
        .await
        .unwrap();

    assert_eq!(spawned.spawned_by, Some(node.id));
}

// ── Incognito: RAM-only sessions ─────────────────────────────────────────────────────────────
//
// The load-bearing claim of incognito mode is negative — that nothing reaches the disk — and a
// negative claim is exactly the kind that rots silently, because no feature stops working when it
// breaks. These assert on the filesystem itself rather than on any store API, so they keep holding
// no matter how the store is refactored underneath.

/// A conversation opened incognito.
fn new_incognito(title: &str) -> NewConversation {
    NewConversation {
        title: Some(title.to_string()),
        parent_conversation: None,
        spawned_by: None,
        ephemeral: true,
        visibility: Default::default(),
        grant: Default::default(),
    }
}

/// Every `*.jsonl` in `dir`, as a sorted list of file stems (which are session ids).
fn logs_in(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
        .filter_map(|e| e.path().file_stem()?.to_str().map(str::to_string))
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn incognito_session_writes_no_file_at_all() {
    let dir = tempdir().unwrap();
    let store = store_at(dir.path()).await;

    let ghost = store.create(new_incognito("private")).await.unwrap();
    let node = store
        .append(ghost.id, user_node(None, "something sensitive"))
        .await
        .unwrap();
    // Retitling appends a fresh header line — a second write path, and the one most likely to be
    // forgotten if the flag were threaded through call sites instead of checked at the chokepoint.
    store
        .set_title(ghost.id, "still private".into())
        .await
        .unwrap();

    assert_eq!(
        logs_in(dir.path()),
        Vec::<String>::new(),
        "an incognito session must not leave a log behind — not a header, not a node, not a retitle"
    );

    // ...and it is fully usable in memory while it exists.
    let path = store.leaf_path(ghost.id, None).await.unwrap();
    assert_eq!(path.len(), 1);
    assert_eq!(path[0].id, node.id);
}

#[tokio::test]
async fn a_normal_session_alongside_an_incognito_one_is_still_durable() {
    // Guards the obvious way to get this wrong: making the *store* ephemeral instead of the session.
    let dir = tempdir().unwrap();
    let store = store_at(dir.path()).await;

    let kept = store.create(new_convo("keep me")).await.unwrap();
    let ghost = store.create(new_incognito("forget me")).await.unwrap();
    store
        .append(kept.id, user_node(None, "durable"))
        .await
        .unwrap();
    store
        .append(ghost.id, user_node(None, "ephemeral"))
        .await
        .unwrap();

    assert_eq!(logs_in(dir.path()), vec![kept.id.to_string()]);

    // Reopening is the real test of durability, and of the ghost's absence.
    let reopened = store_at(dir.path()).await;
    assert_eq!(reopened.leaf_path(kept.id, None).await.unwrap().len(), 1);
    assert!(
        matches!(
            reopened.leaf_path(ghost.id, None).await,
            Err(StoreError::NotFound(_))
        ),
        "an incognito session must not survive a restart"
    );
}

#[tokio::test]
async fn incognito_sessions_are_hidden_from_listings_but_readable_by_id() {
    let dir = tempdir().unwrap();
    let store = store_at(dir.path()).await;

    let listed = store.create(new_convo("in the sidebar")).await.unwrap();
    let ghost = store
        .create(new_incognito("not in the sidebar"))
        .await
        .unwrap();

    let ids: Vec<Ulid> = store.list().await.unwrap().iter().map(|h| h.id).collect();
    assert_eq!(
        ids,
        vec![listed.id],
        "a listed incognito chat is not incognito"
    );

    // The surface that opened it still has to be able to load it back.
    assert_eq!(store.header(ghost.id).await.unwrap().id, ghost.id);
}

#[tokio::test]
async fn deleting_an_incognito_session_succeeds_and_removes_no_one_elses_log() {
    let dir = tempdir().unwrap();
    let store = store_at(dir.path()).await;

    let kept = store.create(new_convo("bystander")).await.unwrap();
    let ghost = store.create(new_incognito("private")).await.unwrap();
    store
        .append(ghost.id, user_node(None, "hello"))
        .await
        .unwrap();

    // A session with no file must still delete cleanly — this is the path the WebUI takes on the way
    // out, and an error here would surface as a failed teardown for a chat that was fine.
    store.delete(ghost.id).await.unwrap();

    assert!(matches!(
        store.header(ghost.id).await,
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(logs_in(dir.path()), vec![kept.id.to_string()]);
}

#[tokio::test]
async fn sweep_drops_idle_incognito_sessions_and_leaves_everything_else() {
    let dir = tempdir().unwrap();
    let store = store_at(dir.path()).await;

    let durable = store.create(new_convo("durable")).await.unwrap();
    let ghost = store.create(new_incognito("private")).await.unwrap();
    store
        .append(ghost.id, user_node(None, "just said this"))
        .await
        .unwrap();

    // A generous idle window: nothing here is anywhere near that old, so nothing should go.
    assert_eq!(store.sweep_ephemeral(Duration::from_secs(3600)).await, 0);
    assert_eq!(store.header(ghost.id).await.unwrap().id, ghost.id);

    // A zero window makes everything idle. Only the incognito session is eligible.
    assert_eq!(store.sweep_ephemeral(Duration::ZERO).await, 1);
    assert!(matches!(
        store.header(ghost.id).await,
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(store.header(durable.id).await.unwrap().id, durable.id);
    assert_eq!(logs_in(dir.path()), vec![durable.id.to_string()]);
}

#[tokio::test]
async fn forking_an_incognito_session_stays_incognito() {
    // Otherwise `fork` is a laundering path: branch the private chat, and the branch lands on disk
    // carrying a copy of every message in it.
    let dir = tempdir().unwrap();
    let store = store_at(dir.path()).await;

    let ghost = store.create(new_incognito("private")).await.unwrap();
    store
        .append(ghost.id, user_node(None, "secret"))
        .await
        .unwrap();

    let fork = store.fork_session(ghost.id, None, None).await.unwrap();

    assert!(fork.ephemeral);
    assert_eq!(logs_in(dir.path()), Vec::<String>::new());
}
