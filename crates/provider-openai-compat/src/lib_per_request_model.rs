//! Split from `lib.rs` for module-health boundaries.

use super::wire_seam::*;
use super::*;
use liberado_provider::Message;

#[tokio::test]
async fn a_request_model_overrides_the_providers_own() {
    let (server, bodies) = recording_server(ok_reply()).await;
    let provider = OpenAiCompatibleProvider::new("sk-test", "daemon-default", server.uri());
    let request = CompletionRequest::new(vec![Message::user("hi")])
        .with_model(Some("deepseek/deepseek-v4-flash".into()));

    provider.complete(request).await.unwrap();

    assert_eq!(
        bodies.lock().unwrap()[0]["model"],
        json!("deepseek/deepseek-v4-flash"),
        "a profile naming a model must beat the daemon default"
    );
    assert_eq!(
        provider.model(),
        "daemon-default",
        "and must not mutate the provider every other session shares"
    );
}

#[tokio::test]
async fn without_a_request_model_the_provider_still_decides() {
    let (server, bodies) = recording_server(ok_reply()).await;
    let provider = OpenAiCompatibleProvider::new("sk-test", "daemon-default", server.uri());
    provider
        .complete(CompletionRequest::new(vec![Message::user("hi")]))
        .await
        .unwrap();
    assert_eq!(bodies.lock().unwrap()[0]["model"], json!("daemon-default"));
}

/// The hot-swap (`/model` in the TUI) and a profile can disagree. The profile wins: naming a
/// model in config is an explicit per-session choice, the swap is a daemon-wide default.
#[tokio::test]
async fn a_request_model_also_beats_a_hot_swapped_one() {
    let (server, bodies) = recording_server(ok_reply()).await;
    let provider = OpenAiCompatibleProvider::new("sk-test", "original", server.uri());
    provider.set_model("hot-swapped".into());

    provider
        .complete(
            CompletionRequest::new(vec![Message::user("hi")])
                .with_model(Some("profile-model".into())),
        )
        .await
        .unwrap();

    assert_eq!(bodies.lock().unwrap()[0]["model"], json!("profile-model"));
}
