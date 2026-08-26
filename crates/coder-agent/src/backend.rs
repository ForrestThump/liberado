//! The Liberado loop backend: the crate's home-spun `CoderBackend` implementation.
//!
//! This file owns the *type* — construction, identity, and top-level run
//! orchestration. The attempt loop, gates, critics, and repair machinery it calls are
//! methods on the same struct declared in the crate root; they stay there because they
//! share private helpers that would otherwise need churning through visibility
//! changes. Splitting the type out of the root keeps the root's module-health ratchet
//! meaningful without hiding anything a reader needs.

use std::sync::Arc;

use async_trait::async_trait;
use liberado_coder_core::{
    CoderBackend, CoderError, CoderRunRequest, CoderRunResult, LIBERADO_LOOP_BACKEND,
};
use liberado_provider::Provider;

use super::{CoderProviderFactory, SingleProviderFactory};
use crate::extension;

/// Liberado's home-spun coding goal-session backend (`CoderBackend` implementation).
#[derive(Clone)]
pub struct LiberadoLoopBackend {
    pub(crate) providers: Arc<dyn CoderProviderFactory>,
    /// Composition-root tools attached to every run this backend executes (see
    /// [`extension::RuntimeExtension`]). Empty for local use.
    pub(crate) extensions: Vec<Arc<dyn extension::RuntimeExtension>>,
}

impl LiberadoLoopBackend {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self::with_provider_factory(Arc::new(SingleProviderFactory::new(provider)))
    }

    pub fn with_provider_factory(providers: Arc<dyn CoderProviderFactory>) -> Self {
        Self {
            providers,
            extensions: Vec::new(),
        }
    }

    /// Offer an extra tool on every run (e.g. the worker's `ask_delegator`).
    pub fn with_extension(mut self, extension: Arc<dyn extension::RuntimeExtension>) -> Self {
        self.extensions.push(extension);
        self
    }

    pub(crate) fn backend_name(&self) -> &'static str {
        LIBERADO_LOOP_BACKEND
    }
}

#[async_trait]
impl CoderBackend for LiberadoLoopBackend {
    fn name(&self) -> &str {
        LIBERADO_LOOP_BACKEND
    }

    async fn run(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
        // The attempt loop is separated from what comes after it so the session critic reads the
        // *whole* run — including every repair attempt. The repair turns are the ones worth
        // reading: an agent answering review feedback is under the most pressure to say "good
        // catch, fixed" and move on, which is exactly the shape of the failure this looks for.
        let config = request.config.clone();
        let mut result = self.run_attempts(request).await?;
        self.review_session_after_run(&config, &mut result).await;
        Ok(result)
    }
}
