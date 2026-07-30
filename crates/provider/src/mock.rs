//! A scriptable mock provider for deterministic tests (Decision 16).
//!
//! Returns canned [`CompletionResponse`]s (and optionally errors) in order and records every
//! [`CompletionRequest`] it received, so a scenario can assert *both* on what the system did with a
//! response *and* on what it sent (e.g. that the dispatcher requested JSON output, or offered the
//! right tools).

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::error::{ProviderError, ProviderResult};
use crate::provider::Provider;
use crate::types::{CompletionRequest, CompletionResponse};

/// A single entry in the mock's script: either a success response or an error.
enum ScriptEntry {
    Ok(CompletionResponse),
    Err(ProviderError),
}

/// A test double for [`Provider`]. Hand it a script of responses (and optionally errors); it pops
/// one per `complete` call and remembers the requests.
pub struct MockProvider {
    model: Mutex<String>,
    scripted: Mutex<VecDeque<ScriptEntry>>,
    received: Mutex<Vec<CompletionRequest>>,
    /// Optional scripted catalog for [`Provider::list_models`] (tests / offline UI).
    models: Mutex<Vec<String>>,
}

impl MockProvider {
    /// A mock that will return `MockExhausted` until responses are [`push`](Self::push)ed.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: Mutex::new(model.into()),
            scripted: Mutex::new(VecDeque::new()),
            received: Mutex::new(Vec::new()),
            models: Mutex::new(Vec::new()),
        }
    }

    /// Set the model ids returned by [`Provider::list_models`].
    pub fn set_models(&self, models: impl IntoIterator<Item = impl Into<String>>) {
        *self.models.lock().unwrap() = models.into_iter().map(Into::into).collect();
    }

    /// A mock pre-loaded with a script of responses, returned in order.
    pub fn with_script(
        model: impl Into<String>,
        responses: impl IntoIterator<Item = CompletionResponse>,
    ) -> Self {
        let mock = Self::new(model);
        for r in responses {
            mock.push(r);
        }
        mock
    }

    /// Queue another response.
    pub fn push(&self, response: CompletionResponse) {
        self.scripted.lock().unwrap().push_back(ScriptEntry::Ok(response));
    }

    /// Queue an error to be returned on a subsequent `complete` call.
    ///
    /// Use this to test transport failures, rate limiting, empty responses, or invalid
    /// requests without needing real network conditions.
    pub fn push_error(&self, error: ProviderError) {
        self.scripted.lock().unwrap().push_back(ScriptEntry::Err(error));
    }

    /// Number of scripted responses not yet consumed.
    pub fn remaining(&self) -> usize {
        self.scripted.lock().unwrap().len()
    }

    /// A snapshot of the requests received so far, in call order — for assertions.
    pub fn received_requests(&self) -> Vec<CompletionRequest> {
        self.received.lock().unwrap().clone()
    }

    /// The most recent request received, if any.
    pub fn last_request(&self) -> Option<CompletionRequest> {
        self.received.lock().unwrap().last().cloned()
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn model(&self) -> String {
        self.model.lock().unwrap().clone()
    }

    fn set_model(&self, model: String) {
        let model = model.trim();
        if !model.is_empty() {
            *self.model.lock().unwrap() = model.to_string();
        }
    }

    async fn complete(&self, request: CompletionRequest) -> ProviderResult<CompletionResponse> {
        self.received.lock().unwrap().push(request);
        match self.scripted.lock().unwrap().pop_front() {
            Some(ScriptEntry::Ok(response)) => Ok(response),
            Some(ScriptEntry::Err(error)) => Err(error),
            None => Err(ProviderError::MockExhausted),
        }
    }

    async fn list_models(&self) -> ProviderResult<Vec<String>> {
        Ok(self.models.lock().unwrap().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::complete_json;
    use crate::types::{CompletionRequest, Message};
    use serde::Deserialize;

    #[tokio::test]
    async fn returns_scripted_responses_in_order() {
        let mock = MockProvider::with_script(
            "mock-model",
            [
                CompletionResponse::text("first"),
                CompletionResponse::text("second"),
            ],
        );
        assert_eq!(mock.remaining(), 2);

        let r1 = mock
            .complete(CompletionRequest::new(vec![Message::user("hi")]))
            .await
            .unwrap();
        assert_eq!(r1.content.as_deref(), Some("first"));

        let r2 = mock
            .complete(CompletionRequest::new(vec![Message::user("again")]))
            .await
            .unwrap();
        assert_eq!(r2.content.as_deref(), Some("second"));

        // Exhausted.
        assert!(matches!(
            mock.complete(CompletionRequest::new(vec![])).await,
            Err(ProviderError::MockExhausted)
        ));
    }

    #[tokio::test]
    async fn records_received_requests() {
        let mock = MockProvider::with_script("mock-model", [CompletionResponse::text("ok")]);
        let req = CompletionRequest::new(vec![
            Message::system("you are a router"),
            Message::user("go"),
        ])
        .with_temperature(0.0);
        mock.complete(req.clone()).await.unwrap();

        let seen = mock.received_requests();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0], req);
        assert_eq!(mock.last_request().unwrap().temperature, Some(0.0));
    }

    #[tokio::test]
    async fn set_models_is_returned_by_list_models() {
        let mock = MockProvider::new("m");
        mock.set_models(["a", "b"]);
        let models = mock.list_models().await.unwrap();
        assert_eq!(models, vec!["a", "b"]);
    }

    #[test]
    fn set_model_rejects_empty_string() {
        let mock = MockProvider::new("original");
        mock.set_model("".into());
        assert_eq!(mock.model(), "original");
        mock.set_model("new".into());
        assert_eq!(mock.model(), "new");
    }

    #[tokio::test]
    async fn complete_json_deserializes_structured_output() {
        #[derive(Debug, PartialEq, Deserialize)]
        struct Decision {
            action: String,
            confidence: f32,
        }

        let mock = MockProvider::with_script(
            "mock-model",
            [CompletionResponse::text(
                r#"{"action":"execute_direct","confidence":0.9}"#,
            )],
        );

        let schema = serde_json::json!({ "type": "object" });
        let decision: Decision = complete_json(
            &mock,
            CompletionRequest::new(vec![Message::user("classify")]),
            schema,
        )
        .await
        .unwrap();

        assert_eq!(
            decision,
            Decision {
                action: "execute_direct".into(),
                confidence: 0.9
            }
        );

        // The helper must have switched the request to JSON mode.
        let sent = mock.last_request().unwrap();
        assert!(matches!(
            sent.response_format,
            crate::types::ResponseFormat::Json { .. }
        ));
    }

    #[tokio::test]
    async fn push_error_script_injects_errors() {
        let mock = MockProvider::new("m");
        mock.push(CompletionResponse::text("ok"));
        mock.push_error(ProviderError::RateLimited);

        // First call: success.
        let r1 = mock
            .complete(CompletionRequest::new(vec![Message::user("hi")]))
            .await
            .unwrap();
        assert_eq!(r1.content.as_deref(), Some("ok"));

        // Second call: rate-limited.
        let r2 = mock
            .complete(CompletionRequest::new(vec![Message::user("again")]))
            .await;
        assert!(matches!(r2, Err(ProviderError::RateLimited)));

        // Third call: exhausted.
        let r3 = mock.complete(CompletionRequest::new(vec![])).await;
        assert!(matches!(r3, Err(ProviderError::MockExhausted)));
    }

    #[tokio::test]
    async fn push_error_mixed_script_interleaves_success_and_failure() {
        let mock = MockProvider::new("m");
        mock.push(CompletionResponse::text("first"));
        mock.push_error(ProviderError::Transport("network down".into()));
        mock.push(CompletionResponse::text("second"));

        // first
        mock.complete(CompletionRequest::new(vec![Message::user("a")]))
            .await
            .unwrap();
        // transport error
        assert!(matches!(
            mock.complete(CompletionRequest::new(vec![Message::user("b")]))
                .await,
            Err(ProviderError::Transport(_))
        ));
        // second
        mock.complete(CompletionRequest::new(vec![Message::user("c")]))
            .await
            .unwrap();
    }
}
