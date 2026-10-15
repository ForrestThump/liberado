//! Per-turn choice of which provider a profile's model is sent to.
//!
//! The profile name lives on the session. The provider id is looked up here, on
//! every turn, from the table the host last installed. Changing the table (a
//! restarted daemon with an edited profile, or [`ChatSessions::set_profile_route`])
//! fixes a session that already pinned a model. The model slug itself still follows
//! the existing precedence in `sessions.rs`.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use liberado_provider::{
    CompletionRequest, CompletionResponse, Provider, ProviderError, ProviderResult,
};

use super::{ChatSessions, TurnSettings};

/// Live routes. The lock is only the name → id map, so a profile edit does not
/// rebuild the HTTP clients.
#[derive(Clone, Default)]
pub(super) struct ProfileProviderTable {
    routes: Arc<RwLock<HashMap<String, String>>>,
    backends: Arc<HashMap<String, Arc<dyn Provider>>>,
    unavailable: Arc<HashMap<String, String>>,
}

impl ProfileProviderTable {
    fn provider_id(&self, profile: &str) -> Option<String> {
        self.routes
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(profile)
            .cloned()
    }

    fn backend_or_failure(&self, profile: &str, provider_id: &str) -> Arc<dyn Provider> {
        if let Some(provider) = self.backends.get(provider_id) {
            return provider.clone();
        }
        let detail = self
            .unavailable
            .get(provider_id)
            .cloned()
            .unwrap_or_else(|| "no credentials were configured for it".to_string());
        Arc::new(FailingProvider {
            reason: format!(
                "profile '{profile}' provider '{provider_id}' is not available: {detail}"
            ),
        })
    }
}

/// Answers every call with the reason the named provider could not be built.
///
/// A silent fall-through to the default provider would send an OpenRouter slug to
/// MiniMax and surface "unknown model", which is the bug this routing exists to stop.
struct FailingProvider {
    reason: String,
}

#[async_trait]
impl Provider for FailingProvider {
    fn model(&self) -> String {
        "unavailable".into()
    }

    async fn complete(&self, _request: CompletionRequest) -> ProviderResult<CompletionResponse> {
        Err(ProviderError::InvalidRequest(self.reason.clone()))
    }
}

impl ChatSessions {
    /// Replace the profile → provider table. Profiles omitted from `routes` keep
    /// the executor's provider.
    pub fn with_profile_providers(
        mut self,
        routes: HashMap<String, String>,
        backends: HashMap<String, Arc<dyn Provider>>,
        unavailable: HashMap<String, String>,
    ) -> Self {
        self.profile_providers = ProfileProviderTable {
            routes: Arc::new(RwLock::new(routes)),
            backends: Arc::new(backends),
            unavailable: Arc::new(unavailable),
        };
        self
    }

    /// Point `profile` at `provider_id` for later turns. `None` clears the route
    /// so the profile uses the default provider again.
    ///
    /// The backend map is the one installed by [`Self::with_profile_providers`].
    /// An id with no backend fails the turn with the unavailable reason.
    pub fn set_profile_route(&self, profile: &str, provider_id: Option<&str>) {
        let mut routes = self
            .profile_providers
            .routes
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match provider_id {
            Some(id) => {
                routes.insert(profile.to_string(), id.to_string());
            }
            None => {
                routes.remove(profile);
            }
        }
    }

    /// Keep a profile's provider when the profile is renamed. A missing route stays missing.
    pub fn rename_profile_route(&self, from: &str, to: &str) {
        if from == to {
            return;
        }
        let mut routes = self
            .profile_providers
            .routes
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(id) = routes.remove(from) {
            routes.insert(to.to_string(), id);
        }
    }

    /// The executor for this turn: the profile's provider when the live table names
    /// one, otherwise the shared default. The model override is applied after the
    /// swap so a pinned slug still wins over the backend's own default.
    pub(super) fn executor_for_turn(&self, settings: &TurnSettings) -> liberado_executor::Executor {
        let model = settings.model.clone();
        let Some(profile) = settings.profile.as_deref() else {
            return self.executor.with_model(model);
        };
        let Some(provider_id) = self.profile_providers.provider_id(profile) else {
            return self.executor.with_model(model);
        };
        self.executor
            .with_provider(
                self.profile_providers
                    .backend_or_failure(profile, &provider_id),
            )
            .with_model(model)
    }
}
