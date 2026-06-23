//! A scriptable mock provider for deterministic tests (Decision 16).
//!
//! Returns canned [`CompletionResponse`]s in order and records every [`CompletionRequest`] it
//! received, so a scenario can assert *both* on what the system did with a response *and* on
//! what it sent (e.g. that the dispatcher requested JSON output, or offered the right tools).

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::error::{ProviderError, ProviderResult};
use crate::provider::Provider;
use crate::types::{CompletionRequest, CompletionResponse};

/// A test double for [`Provider`]. Hand it a script of responses; it pops one per `complete`
/// call and remembers the requests.
pub struct MockProvider {
    model: String,
    scripted: Mutex<VecDeque<CompletionResponse>>,
    received: Mutex<Vec<CompletionRequest>>,
}

impl MockProvider {
    /// A mock that will return `MockExhausted` until responses are [`push`](Self::push)ed.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            scripted: Mutex::new(VecDeque::new()),
            received: Mutex::new(Vec::new()),
        }
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
        self.scripted.lock().unwrap().push_back(response);
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
    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, request: CompletionRequest) -> ProviderResult<CompletionResponse> {
        self.received.lock().unwrap().push(request);
        self.scripted
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(ProviderError::MockExhausted)
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
}
