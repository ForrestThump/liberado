//! Prompt construction: profile nudge, tool manifest, face vs direct swap.

use super::super::*;
use super::test_fixtures::*;
use crate::HUMAN_INTERFACE_SYSTEM_PROMPT;

/// The nudge must qualify the system prompt, not arrive as if the user said it â€” a model treats
/// those very differently.
#[test]
fn a_prompt_append_lands_after_the_system_prompt_and_before_the_first_user_turn() {
    let mut convo = Conversation::from_history(vec![
        Message::system("base prompt"),
        Message::user("hello"),
        Message::assistant("hi"),
    ]);
    convo.apply_prompt_append(Some("Be terse."));

    let roles: Vec<Role> = convo.messages_for_test().iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        vec![Role::System, Role::System, Role::User, Role::Assistant],
        "the nudge must sit with the system prompt, not among the dialogue"
    );
    assert_eq!(convo.messages_for_test()[1].content, "Be terse.");
}

#[test]
fn an_absent_or_blank_prompt_append_changes_nothing() {
    for extra in [None, Some(""), Some("   \n ")] {
        let mut convo =
            Conversation::from_history(vec![Message::system("base"), Message::user("q")]);
        convo.apply_prompt_append(extra);
        assert_eq!(
            convo.messages_for_test().len(),
            2,
            "blank nudge must not add a message: {extra:?}"
        );
    }
}

// ── The prompt must follow the profile ───────────────────────────────────────────────────────────
//
// Found live on 2026-07-28, not by CI: a `basic-chat` session (delegation off, five real tools, no
// `delegate`) was still handed the face-agent root prompt — "you are a face agent, not a tool user…
// call the `delegate` tool", plus an instruction not to enumerate its own tools. Asked for its open
// tasks it answered "I'll fetch your open tasks first." and called nothing. The prompt and the tool
// surface were two sources of truth and they drifted the moment step 5 made the surface per-session
// while the prompt stayed daemon-wide.

/// The regression test that matters: assert on what the **provider actually received**, not on the
/// helper that built it. A session that does not delegate must not be told to delegate.
#[tokio::test]
async fn a_non_delegating_session_is_not_told_it_is_a_face_agent() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(MockProvider::with_script(
        "chat",
        [CompletionResponse::text("ok")],
    ));
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let executor = Executor::new(provider.clone(), Budget::default());
    // Delegation mode on, so the *persisted root prompt* is the face-agent one — exactly the live
    // configuration. No hub attached, so this turn does not run as the face agent.
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools)).with_delegation_mode(true);

    let id = sessions
        .create_with_grant(
            None,
            SessionGrant {
                profile: Some("basic-chat".into()),
                delegation: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    sessions
        .turn(id, "What tasks do I have open?")
        .await
        .unwrap();

    let sent = &provider.received_requests()[0].messages[0];
    assert_eq!(sent.role, Role::System);
    assert_ne!(
        sent.content, HUMAN_INTERFACE_SYSTEM_PROMPT,
        "a session that cannot delegate must not be handed the face-agent prompt"
    );
    assert!(
        !sent.content.contains("delegate"),
        "the model must not be instructed to call a tool it does not hold; got: {}",
        &sent.content[..sent.content.len().min(200)]
    );
}

/// The counterpart: a session that *does* delegate must keep the face-agent prompt. A fix that
/// stripped it unconditionally would trade one drift for another.
#[tokio::test]
async fn a_delegating_session_keeps_the_face_agent_prompt() {
    use liberado_session::{GoalSessionHub, GoalSessionStore};
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(MockProvider::with_script(
        "chat",
        [CompletionResponse::text("ok")],
    ));
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let executor = Executor::new(provider.clone(), Budget::default());
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools))
        .with_delegation_mode(true)
        .with_goal_hub(Arc::new(GoalSessionHub::new(GoalSessionStore::new())));

    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "hello").await.unwrap();

    assert_eq!(
        provider.received_requests()[0].messages[0].content,
        HUMAN_INTERFACE_SYSTEM_PROMPT,
        "the face agent must still be told it is one"
    );
}

#[test]
fn the_swap_replaces_the_builtin_face_prompt_only() {
    // The built-in face prompt is swapped...
    let mut convo = Conversation::from_history(vec![
        Message::system(HUMAN_INTERFACE_SYSTEM_PROMPT),
        Message::user("q"),
    ]);
    convo.apply_direct_agent_prompt();
    assert_eq!(convo.messages_for_test()[0].content, DEFAULT_SYSTEM_PROMPT);

    // ...an operator's own prompt is not. They chose that text for every session, and discarding it
    // silently would be the same class of bug pointing the other way.
    let custom = "You are a narrow research assistant. Never speculate.";
    let mut convo = Conversation::from_history(vec![Message::system(custom), Message::user("q")]);
    convo.apply_direct_agent_prompt();
    assert_eq!(convo.messages_for_test()[0].content, custom);

    // ...and a prompt already correct for this path is left exactly as it is.
    let mut convo = Conversation::from_history(vec![
        Message::system(DEFAULT_SYSTEM_PROMPT),
        Message::user("q"),
    ]);
    convo.apply_direct_agent_prompt();
    assert_eq!(convo.messages_for_test()[0].content, DEFAULT_SYSTEM_PROMPT);
}

/// Order is load-bearing: the profile's nudge qualifies whichever base prompt ends up in force, so
/// it has to stay last. Swapping the base after appending would put them the wrong way round.
#[test]
fn the_swap_leaves_the_profile_nudge_after_the_base_prompt() {
    let mut convo = Conversation::from_history(vec![
        Message::system(HUMAN_INTERFACE_SYSTEM_PROMPT),
        Message::user("q"),
    ]);
    convo.apply_direct_agent_prompt();
    convo.apply_prompt_append(Some("Answer directly and briefly."));

    let msgs = convo.messages_for_test();
    assert_eq!(msgs[0].content, DEFAULT_SYSTEM_PROMPT);
    assert_eq!(msgs[1].content, "Answer directly and briefly.");
    assert_eq!(msgs[2].role, Role::User);
}

#[test]
fn the_swap_is_a_no_op_on_an_empty_or_headless_history() {
    let mut empty = Conversation::from_history(vec![]);
    empty.apply_direct_agent_prompt();
    assert!(empty.messages_for_test().is_empty());

    // A history whose first message is not a system prompt must not be rewritten into one.
    let mut headless = Conversation::from_history(vec![Message::user("q")]);
    headless.apply_direct_agent_prompt();
    assert_eq!(headless.messages_for_test()[0].role, Role::User);
    assert_eq!(headless.messages_for_test()[0].content, "q");
}

// ── The tool manifest: one value, two renderings ────────────────────────────────────────────────

/// The property the whole design rests on: the tools **named in the prompt** and the tools **sent in
/// the request** are the same list, because both come off the runtime handed to the executor.
///
/// Asserted as an equality between the two, not as "the prompt mentions calendar-mcp:list" — a
/// substring check would still pass if the prompt named a tool the request omitted, which is exactly
/// vtcode's `prompts.coder` naming `write_file` against a `unified_file` toolset.
#[tokio::test]
async fn the_prompt_names_exactly_the_tools_the_request_carries() {
    use liberado_common::{Capability, CapabilitySet};

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [CompletionResponse::text("ok")],
    ));
    let executor = Executor::new(provider.clone(), Budget::default());
    let sessions = ChatSessions::new(store, executor, Arc::new(OneTool("calendar-mcp:list")))
        .with_guards(
            vec![("calendar-mcp".into(), Consequence::Reversible)],
            CapabilitySet::from_iter([Capability::ExecuteMcp("calendar-mcp".into())]),
            dir.path().join("proposals"),
            ProposalSigner::random(),
        );

    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "what's on my calendar?").await.unwrap();

    let request = &provider.received_requests()[0];
    let carried: Vec<String> = request.tools.iter().map(|t| t.name.clone()).collect();
    assert!(
        !carried.is_empty(),
        "fixture should carry at least one tool"
    );

    let manifest = request
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .find(|c| c.contains("available to you on this turn"))
        .expect("the turn must state which tools it holds");

    for name in &carried {
        assert!(
            manifest.contains(name.as_str()),
            "tool {name} is in the request but missing from the prompt: {manifest}"
        );
    }
    assert!(
        !manifest.contains("write_file"),
        "sanity: the manifest must not invent tools the request does not carry"
    );
}

/// A turn with nothing to call must say so outright. Otherwise the model fills the silence by
/// offering to look something up — the announce-then-stall failure, reached from the other side.
#[test]
fn a_toolless_turn_is_told_not_to_offer_lookups() {
    let mut convo = Conversation::from_history(vec![Message::system("base"), Message::user("q")]);
    convo.apply_available_tools(&[]);
    let stated = &convo.messages_for_test()[1].content;
    assert!(stated.contains("no tools"), "got: {stated}");
    assert!(
        stated.contains("cannot"),
        "an empty manifest must forbid promising a lookup, not merely omit tools: {stated}"
    );
    // Measured live 2026-08-01. Told it had no tools "on this turn", the model deferred instead —
    // "ask me again on the next turn and I'll do a fresh lookup" — which was untrue: the profile
    // lacked the tool entirely, so no later turn would have differed. Accurate about the turn,
    // misleading about the future, and the same announce-then-cannot shape as the original bug.
    assert!(
        stated.contains("asking again later"),
        "an empty manifest must not invite a retry it cannot honour: {stated}"
    );
    // ...while still allowing honest use of what is already in the conversation, which is what the
    // model got right unprompted: it cited the earlier result and labelled it as earlier.
    assert!(stated.contains("not current"), "{stated}");
}

/// It has to beat concrete tool successes sitting further up the transcript, so it goes last —
/// after the profile nudge, immediately before the dialogue.
#[test]
fn the_tool_manifest_is_the_last_word_before_the_dialogue() {
    let mut convo = Conversation::from_history(vec![
        Message::system(HUMAN_INTERFACE_SYSTEM_PROMPT),
        Message::user("earlier"),
        Message::assistant("earlier reply"),
    ]);
    convo.apply_direct_agent_prompt();
    convo.apply_prompt_append(Some("Answer directly and briefly."));
    convo.apply_available_tools(&[ToolDef::new(
        "turbovault:tasks_list",
        "list tasks",
        serde_json::json!({ "type": "object" }),
    )]);

    let msgs = convo.messages_for_test();
    assert_eq!(msgs[0].content, DEFAULT_SYSTEM_PROMPT);
    assert_eq!(msgs[1].content, "Answer directly and briefly.");
    assert!(msgs[2].content.contains("turbovault:tasks_list"));
    assert_eq!(msgs[2].role, Role::System);
    assert_eq!(
        msgs[3].role,
        Role::User,
        "the manifest must be the final system message, not buried among the dialogue"
    );
}

/// The face-agent prompt is the third builder — #43 fixed the dispatcher, #46 fixed the subagent,
/// and nobody had looked at this one (A3). The claim is that the face agent is *already* correct:
/// the base prompt is purely static and always first, and the two varying blocks — the profile
/// nudge and the tool manifest — follow it in that order.
///
/// This drives a real turn and reads the messages the **provider** received, rather than calling
/// `apply_prompt_append` and `apply_available_tools` by hand and asserting the order they were
/// called in. The ordering being checked is a property of `sessions.rs`, not of `Conversation`:
/// hand-built, the fixture would pass with the two calls swapped at the real call site, which is
/// precisely the mistake it exists to catch.
#[tokio::test]
async fn the_face_agent_sends_its_static_prompt_before_anything_that_varies() {
    use liberado_session::{GoalSessionHub, GoalSessionStore};

    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(MockProvider::with_script(
        "chat",
        [CompletionResponse::text("done")],
    ));
    let sessions = ChatSessions::new(
        Arc::new(SessionStore::open(dir.path()).await),
        Executor::new(provider.clone(), Budget::default()),
        Arc::new(NoTools),
    )
    .with_delegation_mode(true)
    .with_goal_hub(Arc::new(GoalSessionHub::new(GoalSessionStore::new())));

    let id = sessions
        .create_with_grant(
            None,
            SessionGrant {
                profile: Some("terse".into()),
                prompt_append: Some("Answer in one sentence.".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    sessions.turn(id, "add milk").await.unwrap();

    let sent = provider
        .last_request()
        .expect("the turn reached the provider");
    let systems: Vec<&str> = sent
        .messages
        .iter()
        .take_while(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect();

    assert_eq!(
        systems.len(),
        3,
        "base prompt, profile nudge, tool manifest — in one unbroken system block: {systems:?}"
    );
    assert_eq!(
        systems[0], HUMAN_INTERFACE_SYSTEM_PROMPT,
        "the one block that never varies between turns must be the cacheable prefix"
    );
    assert!(
        systems[1].contains("Answer in one sentence."),
        "the profile nudge is second: {:?}",
        systems[1]
    );
    assert!(
        systems[2].contains(crate::DELEGATE_TOOL_NAME),
        "the tool manifest varies most, so it goes last: {:?}",
        systems[2]
    );
    assert_eq!(
        sent.messages[3].role,
        Role::User,
        "and nothing that varies is buried among the dialogue"
    );
}

/// The stale-evidence case: a transcript containing a successful call to a since-revoked tool must
/// be explicitly outranked, not merely contradicted by omission.
#[test]
fn the_manifest_tells_the_model_to_distrust_the_transcript() {
    let mut convo = Conversation::from_history(vec![Message::system("base"), Message::user("q")]);
    convo.apply_available_tools(&[ToolDef::new(
        "search",
        "search",
        serde_json::json!({ "type": "object" }),
    )]);
    let stated = &convo.messages_for_test()[1].content;
    assert!(
        stated.contains("withdrawn") && stated.contains("trust this list"),
        "a tool absent here but present in history must be addressed head-on: {stated}"
    );
}
