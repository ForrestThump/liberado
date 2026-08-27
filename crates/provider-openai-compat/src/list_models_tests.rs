//! `list_models` status handling against a live HTTP seam.
//!
//! Lives outside `lib.rs` because that file sits over the module-health review
//! boundary — any addition there regresses the ratchet (see `module-health.toml`).

use super::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A non-success status must surface as a typed client error — not Ok-with-garbage and not a
/// generic transport failure. This is the regression test for the mutation that removes the
/// error path in `list_models`.
#[tokio::test]
async fn list_models_returns_error_on_non_success_status() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "message": "unauthorized", "type": "invalid_request_error" }
        })))
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new("sk-test", "test-model", server.uri());
    let err = provider.list_models().await.unwrap_err();
    assert!(
        matches!(err, ProviderError::InvalidRequest(_)),
        "expected InvalidRequest for 401, got {err:?}"
    );
}
