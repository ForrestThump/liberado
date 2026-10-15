//! Swap the backend on one executor copy without adding a method to `lib.rs`.

use std::sync::{Arc, Mutex};

use liberado_provider::Provider;

use super::Executor;

impl Executor {
    /// A copy that sends its calls to `provider`. The model override is kept.
    ///
    /// Same reason as [`Executor::with_model`](Executor::with_model): the daemon
    /// holds one shared executor, and one session's profile must not change the
    /// backend under every other session.
    #[must_use]
    pub fn with_provider(&self, provider: Arc<dyn Provider>) -> Self {
        Self {
            provider,
            reasoning: Arc::new(Mutex::new(Vec::new())),
            ..self.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use liberado_provider::MockProvider;

    use super::super::Budget;
    use super::*;

    #[test]
    fn with_provider_replaces_the_backend_and_keeps_a_model_override() {
        let primary = Arc::new(MockProvider::new("primary"));
        let other = Arc::new(MockProvider::new("other"));
        let exec = Executor::new(primary, Budget::default())
            .with_provider(other)
            .with_model(Some("deepseek/deepseek-v4-flash".into()));
        assert_eq!(exec.provider.model(), "other");
        assert_eq!(exec.model.as_deref(), Some("deepseek/deepseek-v4-flash"));
    }
}
