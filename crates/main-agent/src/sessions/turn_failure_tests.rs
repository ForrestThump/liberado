//! A failed turn writes one short note and emits that same sentence.

use super::super::turn_failure::{TURN_ERROR_AUTHOR, public_turn_failure};
use super::super::*;
use super::test_fixtures::*;
use liberado_executor::AgentEvent;

const TOKEN: &str = "123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw";

fn transport_with_token() -> SessionError {
    SessionError::Exec(ExecError::Provider(ProviderError::Transport(format!(
        "https://api.telegram.org/bot{TOKEN}/getUpdates"
    ))))
}

#[test]
fn the_public_sentence_does_not_echo_the_cause() {
    let transport = public_turn_failure(&transport_with_token());
    assert_eq!(
        transport,
        "The model provider failed before it could reply."
    );
    assert!(!transport.contains(TOKEN), "{transport}");

    let unknown = SessionError::Exec(ExecError::Provider(ProviderError::InvalidRequest(format!(
        "unknown model '{TOKEN}'"
    ))));
    let unknown_text = public_turn_failure(&unknown);
    assert_eq!(
        unknown_text,
        "The model rejected this request. Check the model name and try again."
    );
    assert!(!unknown_text.contains("AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw"));

    let internal = SessionError::Exec(ExecError::Internal("tool crashed"));
    assert_eq!(
        public_turn_failure(&internal),
        "This turn failed before it could finish."
    );
}

struct FailingSessions {
    sessions: Arc<ChatSessions>,
    _dir: tempfile::TempDir,
}

async fn sessions_that_fail(error: ProviderError) -> FailingSessions {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::new("mock"));
    provider.push_error(error);
    let executor = Executor::new(provider, Budget::default());
    FailingSessions {
        sessions: Arc::new(ChatSessions::new(
            store,
            executor,
            Arc::new(no_tools_runtime()),
        )),
        _dir: dir,
    }
}

async fn wait_for_error(rx: &mut tokio::sync::broadcast::Receiver<AgentEvent>) -> String {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            match rx.recv().await {
                Ok(AgentEvent::Error(message)) => return message,
                Ok(_) => continue,
                Err(error) => panic!("turn channel ended before the error event: {error}"),
            }
        }
    })
    .await
    .expect("the failed turn emits an error event")
}

#[tokio::test]
async fn a_provider_failure_is_persisted_and_emitted_without_the_secret() {
    let failing = sessions_that_fail(ProviderError::Transport(format!(
        "https://api.telegram.org/bot{TOKEN}/getUpdates"
    )))
    .await;
    let sessions = &failing.sessions;
    let id = sessions.create(None).await.unwrap();
    let (_replay, mut rx) = sessions.start_or_attach(id, "hello");

    let emitted = wait_for_error(&mut rx).await;
    assert_eq!(emitted, "The model provider failed before it could reply.");
    assert!(!emitted.contains(TOKEN), "{emitted}");

    let retired = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while sessions.turn_running(id) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        retired.is_ok(),
        "the running entry is retired after the failure"
    );
    assert!(
        !sessions.last_turn_unanswered(id).await,
        "an error note is a reply, so the turn is not unanswered"
    );

    let nodes = sessions.history_nodes(id).await.unwrap();
    let note = nodes
        .iter()
        .find(|node| matches!(&node.author, Author::Named(name) if name == TURN_ERROR_AUTHOR))
        .expect("the failure note is on the transcript");
    assert_eq!(note.message.content, emitted);
    assert!(
        !note
            .message
            .content
            .contains("AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw")
    );
}

#[tokio::test]
async fn an_unknown_model_persists_the_rejection_sentence() {
    let failing =
        sessions_that_fail(ProviderError::InvalidRequest("unknown model 'nope'".into())).await;
    let sessions = &failing.sessions;
    let id = sessions.create(None).await.unwrap();
    let result = sessions.turn(id, "hello").await.unwrap_err();
    assert_eq!(
        public_turn_failure(&result),
        "The model rejected this request. Check the model name and try again."
    );
    let nodes = sessions.history_nodes(id).await.unwrap();
    assert!(nodes.iter().any(|node| {
        matches!(&node.author, Author::Named(name) if name == TURN_ERROR_AUTHOR)
            && node.message.content == public_turn_failure(&result)
    }));
}
