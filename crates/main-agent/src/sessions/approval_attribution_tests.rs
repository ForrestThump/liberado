//! A chat's permission request names the component that chat runs as.
//!
//! Approving "Everywhere" must widen the agent that asked, not the shared `dispatcher`
//! ceiling that every chat can already reach through `delegate`.

use super::super::*;
use super::test_fixtures::*;

use liberado_common::{Capability, Consequence, McpDescriptor, Proposal, WriteClass, Zone};
use std::sync::Arc;

/// The live TurboVault surface, classified from the build-time bundled defaults — the same
/// shape `liberado_config::catalog_from_config` produces, so `edit_note` counts as a write
/// and resolves its zone from `path`.
fn vault_descriptor() -> McpDescriptor {
    liberado_common::apply_bundled_classifications(McpDescriptor {
        name: "turbovault".into(),
        description: "test turbovault".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: Some("path".into()),
        write_tools: Vec::new(),
        read_tools: Vec::new(),
        vault_wide_writes: Vec::new(),
        write_path_args: std::collections::BTreeMap::new(),
        admin_tools: Vec::new(),
    })
}

/// A chat whose `researcher` profile borrows the `research` grant, holding `turbovault` but no
/// write to the `Tasks` zone — so an `edit_note` there raises a permission request.
async fn researcher_chat(dir: &std::path::Path, resolver_wired: bool) -> ChatSessions {
    let store = Arc::new(SessionStore::open(dir).await);
    let executor = Executor::new(
        Arc::new(MockProvider::with_script(
            "mock",
            Vec::<CompletionResponse>::new(),
        )),
        Budget::default(),
    );
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:edit_note"],
        Ok("edited".into()),
    ));
    let sessions = ChatSessions::new(store, executor, inner)
        .with_guards(
            vec![("turbovault".into(), Consequence::Reversible)],
            CapabilitySet::from_iter([Capability::ExecuteMcp("turbovault".into())]),
            dir.join("proposals"),
            ProposalSigner::random(),
        )
        .with_zone_guards(
            vec![vault_descriptor()],
            vec![("Tasks".to_string(), WriteClass::AgentWritable)],
        );
    if resolver_wired {
        sessions.with_profile_components(Arc::new(|name: &str| match name {
            "researcher" => Some("research".to_string()),
            _ => None,
        }))
    } else {
        sessions
    }
}

/// Raise one permission request from a `researcher` chat and return the proposal note it wrote.
async fn request_raised_by_the_researcher(dir: &std::path::Path, resolver_wired: bool) -> Proposal {
    let sessions = researcher_chat(dir, resolver_wired).await;
    let session = sessions
        .create_with_grant(
            None,
            SessionGrant {
                capabilities: CapabilitySet::from_iter([Capability::ExecuteMcp(
                    "turbovault".into(),
                )]),
                profile: Some("researcher".into()),
                ..Default::default()
            },
        )
        .await
        .expect("the researcher chat starts");

    let grant = sessions.turn_settings(session).await.capabilities;
    let runtime =
        sessions.build_turn_runtime("file this note", session, &[], &grant, Some("researcher"));
    let answer = runtime
        .invoke(&ToolInvocation::new(
            "c-write",
            "turbovault:edit_note",
            serde_json::json!({ "path": "Tasks/Main.md", "content": "x" }),
        ))
        .await
        .expect("an ungranted zone asks rather than failing the turn");
    assert!(
        answer.contains("PERMISSION REQUESTED"),
        "the write must raise a permission request: {answer}"
    );

    let notes = dir.join("proposals").join("proposals");
    let mut entries = std::fs::read_dir(&notes)
        .unwrap_or_else(|err| panic!("no proposals under {}: {err}", notes.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    assert_eq!(entries.len(), 1, "exactly one request: {entries:?}");
    Proposal::from_note(&std::fs::read_to_string(&entries[0]).expect("read the note"))
        .expect("the note parses")
}

/// **Done test (A0b).** A request raised by the researcher names `research`, so approving
/// "Everywhere" widens the researcher. It must not name, or silently fall back to, the shared
/// `dispatcher` ceiling.
#[tokio::test]
async fn a_researcher_request_widens_research_not_dispatcher() {
    let dir = tempfile::tempdir().unwrap();
    let proposal = request_raised_by_the_researcher(dir.path(), true).await;

    assert_eq!(
        proposal.grant_component.as_deref(),
        Some("research"),
        "the request must carry the asking chat's own component"
    );
    assert_ne!(
        proposal.grant_component.as_deref(),
        Some("dispatcher"),
        "approving the researcher must not widen the shared dispatcher ceiling"
    );
    // The pool is unchanged: it still routes the approved call to the default orchestrator.
    // Where the call *runs* and whose authority it *widens* are different questions.
    assert_eq!(
        proposal.pool.as_deref(),
        Some(liberado_common::DEFAULT_POOL)
    );
}

/// The component is signed. A rewrite between propose and approve must fail verification, the
/// same as a rewritten capability — otherwise the field is a suggestion, not an attribution.
#[tokio::test]
async fn rewriting_the_grant_component_breaks_the_signature() {
    let signer = ProposalSigner::random();
    let mut proposal = Proposal::pending(
        "p1",
        "c1",
        "liberado-chat",
        liberado_common::ProposedAction::VaultWrite {
            path: "Tasks/Main.md".into(),
            content_summary: "x".into(),
        },
        "needs a write",
    )
    .with_requested_grant(Capability::Write(Zone::vault("Tasks")));
    proposal.grant_component = Some("research".into());
    let mut signed = signer.sign(proposal).into_proposal();
    assert!(signer.verify(&signed));

    signed.grant_component = Some("dispatcher".into());
    assert!(
        !signer.verify(&signed),
        "redirecting an approval to a wider component must be as evident as a forged capability"
    );
}

/// Without a wired resolver the request carries no component, which leaves the daemon on its
/// pool-keyed fallback. Fail-soft, not fail-open: the fallback is today's behavior, not a wider one.
#[tokio::test]
async fn an_unwired_resolver_leaves_the_component_unset() {
    let dir = tempfile::tempdir().unwrap();
    let proposal = request_raised_by_the_researcher(dir.path(), false).await;
    assert_eq!(proposal.grant_component, None);
}
