//! Integration tests for [`JsonlStore`]: the schema round-trips, the DAG traversal, the
//! file-order == id-order invariant, durability across instances, and concurrent appends.

use std::sync::Arc;

use liberado_conversation_store::{
    Author, ConversationStore, JsonlStore, NewConversation, NewNode, StoreError,
};
use liberado_provider::{Message, ToolInvocation};
use tempfile::tempdir;

/// A conversation with no lineage and the given title.
fn new_convo(title: &str) -> NewConversation {
    NewConversation {
        title: Some(title.to_string()),
        parent_conversation: None,
        spawned_by: None,
    }
}

/// A user node replying to `parent`.
fn user_node(parent: Option<ulid::Ulid>, content: &str) -> NewNode {
    NewNode {
        parent_id: parent,
        author: Author::User,
        message: Message::user(content),
    }
}

#[tokio::test]
async fn create_then_list_returns_the_header() {
    let dir = tempdir().unwrap();
    let store = JsonlStore::new(dir.path());

    let header = store.create(new_convo("first chat")).await.unwrap();

    let listed = store.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, header.id);
    assert_eq!(listed[0].title.as_deref(), Some("first chat"));
}

#[tokio::test]
async fn linear_append_yields_ordered_increasing_path() {
    let dir = tempdir().unwrap();
    let store = JsonlStore::new(dir.path());
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
    let store = JsonlStore::new(dir.path());
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
    let store = JsonlStore::new(dir.path());
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
    let store = JsonlStore::new(dir.path());
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
    let store = JsonlStore::new(dir.path());
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
    let store = JsonlStore::new(dir.path());
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
        let store = JsonlStore::new(dir.path());
        let convo = store.create(new_convo("durable")).await.unwrap().id;
        let n = store
            .append(convo, user_node(None, "persist me"))
            .await
            .unwrap();
        (convo, n)
    }; // first store dropped — only disk remains

    // A brand-new instance over the same root must see the on-disk data.
    let reopened = JsonlStore::new(dir.path());
    let path = reopened.leaf_path(convo, None).await.unwrap();
    assert_eq!(path.len(), 1);
    assert_eq!(path[0], appended);

    let listed = reopened.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, convo);
}

#[tokio::test]
async fn concurrent_appends_to_one_conversation_are_serialized() {
    let dir = tempdir().unwrap();
    let store = Arc::new(JsonlStore::new(dir.path()));
    let convo = store.create(new_convo("hot")).await.unwrap().id;

    // 50 concurrent appends to the SAME conversation (all roots, to keep the test about the writer
    // lock rather than the DAG shape).
    let count = 50;
    let futures = (0..count).map(|i| {
        let store = store.clone();
        async move {
            store
                .append(convo, user_node(None, &format!("c{i}")))
                .await
                .unwrap()
        }
    });
    let mut appended = futures::future::join_all(futures).await;
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
