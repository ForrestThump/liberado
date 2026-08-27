//! Split from `telegram.rs` for module-health boundaries.

use super::*;
use async_trait::async_trait;
use liberado_executor::{Budget, Executor, ToolRuntime};

/// The deterministic result→reply mapping (no session state) must cover the local arms, and
/// the awaited browser/spawn/fork results must be routed past it (unreachable here).
#[test]
fn static_reply_maps_local_results() {
    let mut ctx = TelegramCommandContext {
        session_id: None,
        messages: Vec::new(),
        conversations: Vec::new(),
        goals_summary: Vec::new(),
        status: None,
        message_count: 0,
    };
    assert_eq!(
        static_reply(CommandResult::Quit, &mut ctx).unwrap(),
        "I'm a long-running bot — I can't quit. Use /new for a fresh chat."
    );
    assert!(
        static_reply(CommandResult::OpenThemeBrowser, &mut ctx).is_none(),
        "OpenThemeBrowser must stay silent (ShowOptions already rendered the list)"
    );
    assert!(static_reply(CommandResult::None, &mut ctx).is_none());

    let shown = static_reply(
        CommandResult::ShowOptions {
            title: "Pick".into(),
            options: vec![
                ("Fork".into(), "f1".into()),
                ("Fresh".into(), String::new()),
            ],
        },
        &mut ctx,
    )
    .unwrap();
    assert!(shown.contains("Pick"), "{shown}");
    assert!(shown.contains("Fork  (f1)"), "{shown}");
    assert!(shown.contains("Fresh"), "{shown}");
}

use liberado_common::Capability;
use liberado_config::Grant;
use liberado_executor::AgentEvent;
use liberado_messaging::ChatSurface;
use liberado_provider::ProviderError;
use liberado_provider::{
    CompletionRequest, CompletionResponse, MockProvider, Provider, ProviderResult, ToolDef,
    ToolInvocation,
};
use liberado_session::{DomainHint, GoalSessionHub, GoalSpec, LifeOpsDemoRunner, SessionGrant};
use liberado_session_store::SessionStore;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

struct NoTools;
#[async_trait]
impl ToolRuntime for NoTools {
    fn catalog(&self) -> Vec<ToolDef> {
        Vec::new()
    }
    async fn invoke(&self, _: &ToolInvocation) -> Result<String, String> {
        Err("no tools".into())
    }
}

/// Provider that never completes — keeps a turn in the running map for cancel/lifecycle tests.
struct PendingProvider {
    model: std::sync::Mutex<String>,
    entered: AtomicBool,
}
impl PendingProvider {
    fn new(model: &str) -> Self {
        Self {
            model: std::sync::Mutex::new(model.into()),
            entered: AtomicBool::new(false),
        }
    }
}
#[async_trait]
impl Provider for PendingProvider {
    fn model(&self) -> String {
        self.model.lock().unwrap().clone()
    }
    fn set_model(&self, model: String) {
        *self.model.lock().unwrap() = model;
    }
    async fn complete(&self, _: CompletionRequest) -> ProviderResult<CompletionResponse> {
        self.entered.store(true, Ordering::SeqCst);
        std::future::pending().await
    }
}

/// Hangs on the first completion, answers every one after. Lets a test cancel a turn and then
/// take a *successful* next turn — the sequence the unanswered-turn note exists for.
struct HangOnceProvider {
    model: std::sync::Mutex<String>,
    hung: AtomicBool,
}
#[async_trait]
impl Provider for HangOnceProvider {
    fn model(&self) -> String {
        self.model.lock().unwrap().clone()
    }
    fn set_model(&self, model: String) {
        *self.model.lock().unwrap() = model;
    }
    async fn complete(&self, _: CompletionRequest) -> ProviderResult<CompletionResponse> {
        if !self.hung.swap(true, Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        Ok(CompletionResponse::text("recovered"))
    }
}

/// Spin until the sticky conversation has a turn registered, or give up.
async fn wait_for_running(bridge: &TelegramChatBridge, chat: &Arc<ChatSessions>) -> Ulid {
    for _ in 0..200 {
        if let Some(s) = bridge.session_id.get().await
            && chat.turn_running(s)
        {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no turn registered as running");
}

async fn bridge_with_provider(
    root: &std::path::Path,
    provider: Arc<dyn Provider>,
) -> (TelegramChatBridge, Arc<ChatSessions>, Arc<dyn Provider>) {
    let store = Arc::new(SessionStore::open(root).await);
    let executor = Executor::new(Arc::clone(&provider), Budget::default());
    let chat = Arc::new(ChatSessions::new(
        store.clone(),
        executor,
        Arc::new(NoTools),
    ));
    let mut state = crate::state::AppState::for_test(store, Some(Arc::clone(&chat)), root.into());
    state.provider = Some(Arc::clone(&provider));
    let bridge = TelegramChatBridge {
        state: Arc::new(state),
        session_id: StickySession::ephemeral(),
    };
    (bridge, chat, provider)
}

/// Same as [`bridge_with_provider`] but with an explicit config — the `/spawn` refusal branches
/// depend on which profiles and grants are configured.
async fn bridge_with_config(
    root: &std::path::Path,
    provider: Arc<dyn Provider>,
    config: Arc<liberado_bootstrap::Config>,
) -> (TelegramChatBridge, Arc<ChatSessions>, Arc<dyn Provider>) {
    let store = Arc::new(SessionStore::open(root).await);
    let executor = Executor::new(Arc::clone(&provider), Budget::default());
    let chat = Arc::new(ChatSessions::new(
        store.clone(),
        executor,
        Arc::new(NoTools),
    ));
    let mut state = crate::state::AppState::for_test(store, Some(Arc::clone(&chat)), root.into());
    state.provider = Some(Arc::clone(&provider));
    state.config = config;
    let bridge = TelegramChatBridge {
        state: Arc::new(state),
        session_id: StickySession::ephemeral(),
    };
    (bridge, chat, provider)
}

/// Like [`bridge_with_provider`] but with a goal-session hub that has the life demo pack
/// registered, so a test can start a real goal session and exercise `/join`.
async fn bridge_with_goal_pack(
    root: &std::path::Path,
    provider: Arc<dyn Provider>,
) -> (TelegramChatBridge, Arc<ChatSessions>, Arc<GoalSessionHub>) {
    let store = Arc::new(SessionStore::open(root).await);
    let executor = Executor::new(Arc::clone(&provider), Budget::default());
    let chat = Arc::new(ChatSessions::new(
        store.clone(),
        executor,
        Arc::new(NoTools),
    ));
    let mut hub = GoalSessionHub::new(SessionStore::clone(&store));
    hub.register_pack(Arc::new(LifeOpsDemoRunner));
    let goals = Arc::new(hub);
    let mut state = crate::state::AppState::for_test(store, Some(Arc::clone(&chat)), root.into());
    state.provider = Some(provider);
    state.goals = goals.clone();
    let bridge = TelegramChatBridge {
        state: Arc::new(state),
        session_id: StickySession::ephemeral(),
    };
    (bridge, chat, goals)
}

/// R3: `/model <id>` with a sticky chat sets the **next turn's user-node model stamp**, not
/// merely a reply string. Also asserts the process-wide provider default is unchanged.
///
/// R1: if `select_model` is reverted to only `provider.set_model`, the stamp assertion fails
/// (pending pick never lands) and/or the global-default assertion fails.
#[tokio::test]
async fn model_command_scopes_to_sticky_and_stamps_the_next_turn() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockProvider::with_script(
        "daemon-default",
        [
            CompletionResponse::text("ok"),
            CompletionResponse::text("second"),
        ],
    ));
    let (bridge, chat, provider) = bridge_with_provider(dir.path(), mock).await;

    // Establish sticky conversation (same get_or_create path free-form uses).
    let first = bridge.reply("hello").await.unwrap();
    assert_eq!(first, "ok");
    let sticky = bridge
        .session_id
        .get()
        .await
        .expect("sticky after first turn");
    let global_before = provider.model();
    assert_eq!(global_before, "daemon-default");

    let reply = bridge.reply("/model picked/for-telegram").await.unwrap();
    assert!(
        reply.contains("this Telegram chat") || reply.contains(&sticky.to_string()),
        "reply must state conversation scope: {reply}"
    );
    assert_eq!(
        provider.model(),
        global_before,
        "/model must not change the process-wide default while sticky exists"
    );

    // Next turn must run on the pick — assert the stamp on the user node (R3), not reply text.
    bridge.reply("next turn").await.unwrap();
    let nodes = chat.history_nodes(sticky).await.unwrap();
    let user = nodes
        .iter()
        .rev()
        .find(|n| matches!(n.author, Author::User))
        .expect("user node for the second turn");
    assert_eq!(
        user.model.as_deref(),
        Some("picked/for-telegram"),
        "next turn must stamp the per-conversation pick, not the daemon default"
    );
    assert_eq!(
        provider.model(),
        "daemon-default",
        "process-wide default must still be untouched after the turn"
    );
}

/// No sticky yet: /model creates a conversation and scopes there — stated, not silent global.
#[tokio::test]
async fn model_without_sticky_creates_conversation_and_scopes() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockProvider::with_script(
        "daemon-default",
        [CompletionResponse::text("after-model")],
    ));
    let (bridge, chat, provider) = bridge_with_provider(dir.path(), mock).await;
    assert!(bridge.session_id.get().await.is_none());

    let reply = bridge.reply("/model fresh/pick").await.unwrap();
    assert!(
        reply.contains("No chat was open") || reply.contains("started Telegram session"),
        "must state the no-sticky policy: {reply}"
    );
    assert_eq!(
        provider.model(),
        "daemon-default",
        "must not fall back to silent process-wide set_model"
    );
    let sticky = bridge
        .session_id
        .get()
        .await
        .expect("sticky created by /model");
    bridge.reply("go").await.unwrap();
    let nodes = chat.history_nodes(sticky).await.unwrap();
    let user = nodes
        .iter()
        .rev()
        .find(|n| matches!(n.author, Author::User))
        .unwrap();
    assert_eq!(user.model.as_deref(), Some("fresh/pick"));
}

/// Free-form while a sticky turn is running gets a distinguishable response (not the model).
#[tokio::test]
async fn freeform_while_turn_running_is_refused_with_feedback() {
    let dir = tempfile::tempdir().unwrap();
    let pending = Arc::new(PendingProvider::new("pending"));
    let (bridge, chat, _) = bridge_with_provider(dir.path(), pending.clone()).await;

    // Start a hang turn in the background via the real bridge path.
    let b = TelegramChatBridge {
        state: Arc::clone(&bridge.state),
        session_id: bridge.session_id.clone(),
    };
    let hang = tokio::spawn(async move { b.reply("long running").await });

    // Wait until the turn is registered (provider entered or turn_running).
    let sticky = {
        let mut id = None;
        for _ in 0..100 {
            if let Some(s) = bridge.session_id.get().await
                && chat.turn_running(s)
            {
                id = Some(s);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        id.expect("sticky turn should register as running")
    };
    assert!(chat.turn_running(sticky));

    let feedback = bridge.reply("another message").await.unwrap();
    assert!(
        feedback.contains("already running") && feedback.contains("/stop"),
        "must be a lifecycle reply, not a model completion: {feedback}"
    );
    // Not a silent attach that waits on the hang.
    hang.abort();
    let _ = hang.await;
    let _ = chat.cancel_turn(sticky);
}

/// /stop cancels through the real cancel path; reply does not promise a kept partial.
#[tokio::test]
async fn stop_cancels_inflight_turn_without_promising_partial() {
    let dir = tempfile::tempdir().unwrap();
    let pending = Arc::new(PendingProvider::new("pending"));
    let (bridge, chat, _) = bridge_with_provider(dir.path(), pending).await;

    let b = TelegramChatBridge {
        state: Arc::clone(&bridge.state),
        session_id: bridge.session_id.clone(),
    };
    let hang = tokio::spawn(async move { b.reply("hang").await });

    let sticky = {
        let mut id = None;
        for _ in 0..100 {
            if let Some(s) = bridge.session_id.get().await
                && chat.turn_running(s)
            {
                id = Some(s);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        id.expect("running turn")
    };

    let stop_reply = bridge.reply("/stop").await.unwrap();
    assert!(
        stop_reply.to_lowercase().contains("cancel"),
        "stop reply should acknowledge cancel: {stop_reply}"
    );
    assert!(
        !stop_reply.to_lowercase().contains("kept")
            || stop_reply.to_lowercase().contains("nothing")
            || stop_reply.to_lowercase().contains("no partial"),
        "must not promise a partial was kept: {stop_reply}"
    );
    // Stronger: our shipped text says nothing was kept.
    assert!(
        stop_reply.contains("Nothing from that turn was kept")
            || stop_reply.contains("keeps no partial"),
        "honest cancel wording required: {stop_reply}"
    );
    assert!(
        !chat.turn_running(sticky),
        "cancel_turn must clear the running map"
    );

    let hang_result = hang.await.expect("join");
    // The waiting free-form future should fail with cancelled, not return a model answer.
    assert!(
        hang_result.is_err()
            || hang_result
                .as_ref()
                .is_ok_and(|s| s.to_lowercase().contains("cancel")),
        "hung turn after /stop: {hang_result:?}"
    );
}

/// After a cancelled turn, the **next** message says the previous turn ended without a reply.
///
/// This is the second half of the lifecycle acceptance item ("if the last turn ended
/// unanswered, say that instead of silence"), and it was the one path with no test: deleting
/// the whole `unanswered_prefix` block left every other test in this module green.
///
/// R3: crosses from the running-turn map into the persisted log — `last_turn_unanswered` reads
/// message nodes, so this only passes if the cancelled turn genuinely left a user node with no
/// reply after it, not merely if a flag was set in memory.
#[tokio::test]
async fn message_after_a_cancelled_turn_reports_the_unanswered_turn() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(HangOnceProvider {
        model: std::sync::Mutex::new("m".into()),
        hung: AtomicBool::new(false),
    });
    let (bridge, chat, _) = bridge_with_provider(dir.path(), provider).await;

    let b = TelegramChatBridge {
        state: Arc::clone(&bridge.state),
        session_id: bridge.session_id.clone(),
    };
    let hang = tokio::spawn(async move { b.reply("first").await });
    let sticky = wait_for_running(&bridge, &chat).await;

    assert!(bridge.reply("/stop").await.is_ok());
    let _ = hang.await;
    assert!(!chat.turn_running(sticky));
    assert!(
        chat.last_turn_unanswered(sticky).await,
        "precondition: the cancelled turn must leave an unanswered user node"
    );

    let next = bridge.reply("second").await.unwrap();
    assert!(
        next.contains("previous turn ended without a reply"),
        "must state the unanswered turn rather than answering in silence: {next}"
    );
    assert!(
        next.contains("recovered"),
        "the note prefixes the reply, it does not replace it: {next}"
    );
}

/// The note is *conditional* — an ordinary turn following an answered one carries no prefix.
/// Without this, always emitting the note would pass the test above.
#[tokio::test]
async fn ordinary_turn_carries_no_unanswered_note() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockProvider::with_script(
        "m",
        [
            CompletionResponse::text("one"),
            CompletionResponse::text("two"),
        ],
    ));
    let (bridge, _chat, _) = bridge_with_provider(dir.path(), mock).await;
    assert_eq!(bridge.reply("hello").await.unwrap(), "one");
    let second = bridge.reply("again").await.unwrap();
    assert_eq!(
        second, "two",
        "an answered turn must not be decorated: {second}"
    );
}

#[tokio::test]
async fn help_command_lists_available_commands() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockProvider::new("m"));
    let (bridge, _chat, _provider) = bridge_with_provider(dir.path(), mock).await;
    let reply = bridge.reply("/help").await.unwrap();
    // Commands from the shared telegram_commands() catalog.
    for cmd in [
        "/help",
        "/new",
        "/status",
        "/sessions",
        "/spawn",
        "/goal",
        "/join",
        "/model",
        "/fork",
    ] {
        assert!(reply.contains(cmd), "/help must list {cmd}: got {reply}");
    }
    // Telegram-specific commands not in the shared catalog.
    assert!(reply.contains("/stop"), "must include /stop");
    // Shared catalog descriptions come through (not the hardcoded old text).
    assert!(
        !reply.contains("switch to model"),
        "descriptions come from COMMAND_CATALOG, not hardcoded"
    );
}

// --- Mutation-hardening tests: the branches the 2026-07-30 report left uncovered. ---

#[tokio::test]
async fn spawn_naming_an_unknown_profile_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockProvider::new("m"));
    let (bridge, _chat, _) = bridge_with_provider(dir.path(), mock).await;
    let reply = bridge
        .reply("/spawn nosuchprofile write a report")
        .await
        .unwrap();
    assert!(reply.contains("Unknown session profile"), "{reply}");
}

#[tokio::test]
async fn spawn_a_chat_only_profile_is_refused_not_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockProvider::new("m"));
    // `chatpack` is an enabled profile with no domain — a chat-only hat `/spawn` must refuse.
    // Built through TOML so `Config::validate` runs (the profile's component needs a grant).
    let config = liberado_bootstrap::Config::from_str(
        r#"
[topology]
vault_path = "/tmp/vault"

[[topology.session_profiles]]
name = "chatpack"

[[policy.grants]]
component = "chatpack"
capabilities = []
"#,
    )
    .unwrap();
    let (bridge, _chat, _) = bridge_with_config(dir.path(), mock, Arc::new(config)).await;
    let reply = bridge
        .reply("/spawn chatpack write a report")
        .await
        .unwrap();
    assert!(reply.contains("is a chat profile"), "{reply}");
}

#[tokio::test]
async fn spawn_a_domain_with_no_grant_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockProvider::new("m"));
    // Config::default() has no grants, so "coding" resolves to zero authority.
    let (bridge, _chat, _) = bridge_with_provider(dir.path(), mock).await;
    let reply = bridge.reply("/spawn coding write a report").await.unwrap();
    assert!(reply.contains("no capability grant"), "{reply}");
}

#[tokio::test]
async fn spawn_passes_refusals_then_surfaces_a_start_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockProvider::new("m"));
    let mut config = liberado_bootstrap::Config::default();
    config.policy.grants.push(Grant {
        component: "coding".into(),
        capabilities: vec![Capability::AskHuman],
    });
    let (bridge, _chat, _) = bridge_with_config(dir.path(), mock, Arc::new(config)).await;
    let reply = bridge.reply("/spawn coding write a report").await.unwrap();
    // No pack is registered for "coding" in the test hub, so the start must fail loudly.
    assert!(reply.contains("Spawn failed"), "{reply}");
}

#[tokio::test]
async fn fork_without_an_active_session_explains() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockProvider::new("m"));
    let (bridge, _chat, _) = bridge_with_provider(dir.path(), mock).await;
    let reply = bridge.reply("/fork").await.unwrap();
    assert!(reply.contains("No conversation to fork"), "{reply}");
}

#[tokio::test]
async fn fork_after_turn_zero_is_rejected_as_1_based() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockProvider::with_script(
        "m",
        [CompletionResponse::text("ok")],
    ));
    let (bridge, _chat, _) = bridge_with_provider(dir.path(), mock).await;
    assert_eq!(bridge.reply("hello").await.unwrap(), "ok");
    let reply = bridge.reply("/fork 0").await.unwrap();
    assert!(reply.contains("Turns are numbered from 1"), "{reply}");
}

#[tokio::test]
async fn fork_branches_the_sticky_conversation_and_switches_to_it() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockProvider::with_script(
        "m",
        [CompletionResponse::text("ok")],
    ));
    let (bridge, chat, _) = bridge_with_provider(dir.path(), mock).await;
    assert_eq!(bridge.reply("hello").await.unwrap(), "ok");
    let sticky = bridge.session_id.get().await.expect("sticky after turn");

    let reply = bridge.reply("/fork").await.unwrap();
    assert!(reply.contains("Forked"), "{reply}");
    let fork_id = bridge.session_id.get().await.expect("switched to the fork");
    assert_ne!(fork_id, sticky, "the fork must be a new conversation");

    // The fork copied the transcript — one user turn, its reply, and the original untouched.
    let nodes = chat.history_nodes(fork_id).await.unwrap();
    assert_eq!(
        nodes
            .iter()
            .filter(|n| matches!(n.author, Author::User))
            .count(),
        1,
        "fork must carry the user turn"
    );
    assert!(
        !chat.turn_running(sticky),
        "forking must not disturb the original conversation"
    );

    // /fork 1 keeps through the first turn — same single-turn transcript, fresh id.
    let before = bridge.session_id.get().await.unwrap();
    let reply = bridge.reply("/fork 1").await.unwrap();
    assert!(reply.contains("kept_turns=1/1"), "{reply}");
    assert_ne!(bridge.session_id.get().await.unwrap(), before);
}

#[tokio::test]
async fn join_snapshot_renders_a_live_goal_session() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockProvider::new("m"));
    let (bridge, _chat, goals) = bridge_with_goal_pack(dir.path(), mock).await;
    let id = goals
        .start_with_grant(
            GoalSpec {
                id: None,
                description: "review the week".into(),
                success_criteria: vec![],
                domain: DomainHint::from("life"),
                max_turns: 0,
                max_idle_secs: None,
                origin: None,
                profile: None,
                payload: serde_json::json!({}),
            },
            SessionGrant::default(),
        )
        .await
        .unwrap();

    let reply = bridge.reply(&format!("/join {id}")).await.unwrap();
    assert!(reply.contains(&format!("Goal session {id}")), "{reply}");
}

#[tokio::test]
async fn join_by_id_prefix_falls_back_to_the_listing() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockProvider::new("m"));
    let (bridge, _chat, goals) = bridge_with_goal_pack(dir.path(), mock).await;
    let id = goals
        .start_with_grant(
            GoalSpec {
                id: None,
                description: "plan the quarter".into(),
                success_criteria: vec![],
                domain: DomainHint::from("life"),
                max_turns: 0,
                max_idle_secs: None,
                origin: None,
                profile: None,
                payload: serde_json::json!({}),
            },
            SessionGrant::default(),
        )
        .await
        .unwrap();
    let prefix: String = id.to_string().chars().take(8).collect();

    let reply = bridge.reply(&format!("/join {prefix}")).await.unwrap();
    assert!(reply.contains(&format!("Goal session {id}")), "{reply}");
}

#[tokio::test]
async fn join_an_unknown_goal_session_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockProvider::new("m"));
    let (bridge, _chat, _goals) = bridge_with_goal_pack(dir.path(), mock).await;
    let reply = bridge.reply("/join zzzzz").await.unwrap();
    assert!(
        reply.contains("No goal session matching 'zzzzz'"),
        "{reply}"
    );
}

#[tokio::test]
async fn session_switch_by_exact_ulid_moves_the_sticky_chat() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockProvider::new("m"));
    let (bridge, chat, _) = bridge_with_provider(dir.path(), mock).await;
    let conv = chat.create(None).await.unwrap();

    let reply = bridge
        .reply(&format!("/session switch {conv}"))
        .await
        .unwrap();
    assert!(
        reply.contains(&format!("Switched to session {conv}")),
        "{reply}"
    );
    assert_eq!(bridge.session_id.get().await.unwrap(), conv);
}

#[tokio::test]
async fn session_switch_resolves_by_prefix_and_reports_no_match() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockProvider::new("m"));
    let (bridge, chat, _) = bridge_with_provider(dir.path(), mock).await;
    let conv = chat.create(None).await.unwrap();
    let prefix: String = conv.to_string().chars().take(8).collect();

    let reply = bridge
        .reply(&format!("/session switch {prefix}"))
        .await
        .unwrap();
    assert!(reply.contains("Switched to session"), "{reply}");
    assert_eq!(bridge.session_id.get().await.unwrap(), conv);

    let reply = bridge.reply("/session switch zzzzz").await.unwrap();
    assert!(reply.contains("No session matching 'zzzzz'"), "{reply}");
}

#[tokio::test]
async fn sessions_browser_lists_chats_and_goal_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockProvider::new("m"));
    let (bridge, chat, _) = bridge_with_provider(dir.path(), mock).await;
    let conv = chat.create(None).await.unwrap();

    let reply = bridge.reply("/sessions").await.unwrap();
    assert!(reply.contains("Sessions (chat):"), "{reply}");
    assert!(reply.contains(&conv.to_string()), "{reply}");
    assert!(reply.contains("Goal sessions:"), "{reply}");
    assert!(reply.contains("(none)"), "{reply}");
}

#[tokio::test]
async fn unknown_slash_command_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockProvider::new("m"));
    let (bridge, _chat, _) = bridge_with_provider(dir.path(), mock).await;
    let reply = bridge.reply("/definitely-not-a-command").await;
    assert!(
        matches!(reply, Err(ref e) if e.contains("Unknown command")),
        "{reply:?}"
    );
}

#[tokio::test]
async fn chat_turn_refuses_when_chat_is_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let state = crate::state::AppState::for_test(store, None, dir.path().into());
    let bridge = TelegramChatBridge {
        state: Arc::new(state),
        session_id: StickySession::ephemeral(),
    };
    let reply = bridge.reply("hello").await;
    assert!(
        matches!(reply, Err(ref e) if e == "chat is disabled"),
        "{reply:?}"
    );
}

#[tokio::test]
async fn model_browser_renders_current_and_listed_models() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockProvider::new("m"));
    mock.set_models(["alpha", "beta"]);
    let (bridge, _chat, _) = bridge_with_provider(dir.path(), mock).await;

    let reply = bridge.reply("/model").await.unwrap();
    assert!(reply.contains("Current model: m"), "{reply}");
    assert!(reply.contains("alpha"), "{reply}");
    assert!(reply.contains("beta"), "{reply}");
}

#[tokio::test]
async fn model_browser_with_no_provider_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let executor = Executor::new(Arc::new(MockProvider::new("m")), Budget::default());
    let chat = Arc::new(ChatSessions::new(
        store.clone(),
        executor,
        Arc::new(NoTools),
    ));
    let mut state = crate::state::AppState::for_test(store, Some(chat), dir.path().into());
    state.provider = None;
    let bridge = TelegramChatBridge {
        state: Arc::new(state),
        session_id: StickySession::ephemeral(),
    };
    let reply = bridge.reply("/model").await.unwrap();
    assert!(reply.contains("No provider configured"), "{reply}");
}

struct ModelListErrorProvider;
#[async_trait]
impl Provider for ModelListErrorProvider {
    fn model(&self) -> String {
        "m".into()
    }
    fn set_model(&self, _: String) {}
    async fn complete(&self, _: CompletionRequest) -> ProviderResult<CompletionResponse> {
        Ok(CompletionResponse::text("ok"))
    }
    async fn list_models(&self) -> ProviderResult<Vec<String>> {
        Err(ProviderError::Transport("list failed".into()))
    }
}

#[tokio::test]
async fn model_browser_surfaces_a_list_models_failure() {
    let dir = tempfile::tempdir().unwrap();
    let (bridge, _chat, _) =
        bridge_with_provider(dir.path(), Arc::new(ModelListErrorProvider)).await;
    let reply = bridge.reply("/model").await.unwrap();
    assert!(reply.contains("Could not list models"), "{reply}");
    assert!(reply.contains("list failed"), "{reply}");
}

// collect_turn_reply is a free function fed a broadcast stream; unit-driving it directly
// covers the event variants the end-to-end turns never reach (Lagged, Closed-with-empty).
#[tokio::test]
async fn collect_turn_reply_surfaces_error_and_discards_stream() {
    let (tx, rx) = tokio::sync::broadcast::channel(8);
    tx.send(AgentEvent::Token("partial".into())).ok();
    tx.send(AgentEvent::Error("boom".into())).ok();
    assert_eq!(collect_turn_reply(rx).await, Err("boom".into()));
}

#[tokio::test]
async fn collect_turn_reply_skips_lagged_and_returns_latest() {
    let (tx, rx) = tokio::sync::broadcast::channel(2);
    tx.send(AgentEvent::Token("a".into())).ok();
    tx.send(AgentEvent::Token("b".into())).ok();
    tx.send(AgentEvent::Token("c".into())).ok();
    tx.send(AgentEvent::Done).ok();
    let reply = collect_turn_reply(rx).await;
    assert_eq!(reply, Ok("c".into()));
}

#[tokio::test]
async fn collect_turn_reply_closed_with_empty_stream_errors() {
    let (tx, rx) = tokio::sync::broadcast::channel(8);
    drop(tx);
    assert_eq!(
        collect_turn_reply(rx).await,
        Err("turn ended without a reply".into())
    );
}

#[tokio::test]
async fn collect_turn_reply_closed_after_partial_returns_partial() {
    let (tx, rx) = tokio::sync::broadcast::channel(8);
    tx.send(AgentEvent::Token("partial".into())).ok();
    drop(tx);
    assert_eq!(collect_turn_reply(rx).await, Ok("partial".into()));
}

#[tokio::test]
async fn collect_turn_reply_done_returns_stream() {
    let (tx, rx) = tokio::sync::broadcast::channel(8);
    tx.send(AgentEvent::Token("hi".into())).ok();
    tx.send(AgentEvent::Done).ok();
    assert_eq!(collect_turn_reply(rx).await, Ok("hi".into()));
}

#[tokio::test]
async fn collect_turn_reply_ignores_non_token_events() {
    let (tx, rx) = tokio::sync::broadcast::channel(8);
    tx.send(AgentEvent::ToolStarted {
        name: "x".into(),
        args: "y".into(),
    })
    .ok();
    tx.send(AgentEvent::Done).ok();
    assert_eq!(collect_turn_reply(rx).await, Ok(String::new()));
}

/// The deterministic result→reply mapping must cover every surface-local arm, including the
/// lifecycle verbs that reset the sticky session. (Browser/fork/spawn results are routed past
/// `static_reply` and asserted via the end-to-end tests above.)
#[test]
fn static_reply_covers_surface_local_arms() {
    let ctx = || TelegramCommandContext {
        session_id: Some("sess-1".into()),
        messages: Vec::new(),
        conversations: Vec::new(),
        goals_summary: Vec::new(),
        status: None,
        message_count: 0,
    };

    let mut c = ctx();
    let r = static_reply(
        CommandResult::NewConversation {
            was_streaming: false,
        },
        &mut c,
    )
    .unwrap();
    assert!(r.contains("Started a new conversation"), "{r}");
    assert!(c.session_id.is_none(), "/new must clear the sticky session");

    let r = static_reply(CommandResult::ChatCleared, &mut ctx()).unwrap();
    assert!(r.contains("no local transcript buffer"), "{r}");

    let mut c = ctx();
    let r = static_reply(
        CommandResult::SessionClosed {
            id: Some("abc".into()),
        },
        &mut c,
    )
    .unwrap();
    assert!(r.contains("Closed session abc"), "{r}");
    assert!(
        c.session_id.is_none(),
        "/close must clear the sticky session"
    );

    let r = static_reply(CommandResult::SessionClosed { id: None }, &mut ctx()).unwrap();
    assert!(r.contains("No active session"), "{r}");

    let r = static_reply(CommandResult::BackToPrimary, &mut ctx()).unwrap();
    assert!(r.contains("Back on primary chat"), "{r}");

    let r = static_reply(CommandResult::OpenProfileBrowser, &mut ctx()).unwrap();
    assert!(
        r.contains("Session profiles are switched from the web UI"),
        "{r}"
    );

    for coding in [
        CommandResult::StartCodingGoal {
            project: None,
            text: "x".into(),
            mode: None,
        },
        CommandResult::OpenGoalView,
        CommandResult::GoalStatus,
        CommandResult::ParkGoalSession,
        CommandResult::ResumeGoalSession { answer: "y".into() },
        CommandResult::CancelGoalSession,
    ] {
        let r = static_reply(coding, &mut ctx()).unwrap();
        assert!(r.contains("Coding goals run in the TUI"), "{r}");
    }

    let r = static_reply(
        CommandResult::ThemeChanged {
            name: "dark".into(),
        },
        &mut ctx(),
    )
    .unwrap();
    assert!(r.contains("Theme 'dark' is UI-only"), "{r}");

    let r = static_reply(
        CommandResult::ThemesReloaded {
            count: 1,
            errors: vec![],
        },
        &mut ctx(),
    )
    .unwrap();
    assert!(r.contains("Themes are UI-only on Telegram"), "{r}");

    let r = static_reply(
        CommandResult::ThemeListed {
            names: vec!["a".into()],
            active: "a".into(),
        },
        &mut ctx(),
    )
    .unwrap();
    assert!(r.contains("Themes are UI-only on Telegram"), "{r}");

    for silent in [
        CommandResult::HelpShown,
        CommandResult::StatusShown,
        CommandResult::ModelInfoShown,
        CommandResult::SessionInfoShown,
        CommandResult::ProfileInfoShown,
        CommandResult::OpenThemeBrowser,
    ] {
        assert!(
            static_reply(silent.clone(), &mut ctx()).is_none(),
            "{silent:?}"
        );
    }
}
