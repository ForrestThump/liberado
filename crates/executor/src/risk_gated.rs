//! # RiskGatedToolRuntime
//!
//! A [`ToolRuntime`] wrapper that applies deterministic safety guards (capability / consequence /
//! magnitude) to every tool call **at runtime**, mirroring the dispatcher's pre-flight guard
//! pipeline. High-consequence or sweeping-destructive calls are downgraded to a proposal file
//! written to `proposals_dir/proposals/<id>.md` instead of executing.
//!
//! This is the runtime safety net that sits between the agent loop and the actual MCP tools —
//! even if the dispatcher misclassifies a request, this guard catches it before the tool runs.
//!
//! ## `proposals_dir` is the vault's `proposals/` directory
//!
//! Earlier, downgraded proposals were written outside the vault (a data directory) specifically so
//! the vault watcher would never react to them — but nothing ever read them back either, so an
//! approval had nowhere to go: a genuine dead end. `proposals_dir` is now the **vault's**
//! `proposals/` directory, the same one the dispatcher's pre-flight `Propose` disposition already
//! uses. The daemon's `react()` routes any change under `proposals/` straight to its
//! approve/execute handling based on path alone, so a downgrade written here now flows through the
//! same, already-working propose→approve→execute pipeline — approving one actually executes it.
//!
//! ## Downgrade is a tool *result*, not an error
//!
//! A consequence/magnitude downgrade returns `Ok(<clear message>)` — a tool *result* the model
//! relays cleanly — not `Err`. The executor prefixes `Err` with `"tool error:"`, which the model
//! then awkwardly narrates around; a clear `Ok` message ("PROPOSAL CREATED — not executed …") reads
//! correctly. Only a genuine **capability denial** stays `Err` (the action is refused, not deferred).
//! A dedicated `proposal` SSE event would be a nicer future refinement than overloading the result
//! string, but this keeps the streaming UX clean without a new event type.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use liberado_common::{
    CapabilitySet, Consequence, Proposal, ProposalSigner, ProposedAction,
    is_sweeping_destructive, mcp_of,
};
use liberado_provider::{ToolDef, ToolInvocation};
use tracing::Instrument;

use crate::ToolRuntime;

/// A runtime guard that wraps an inner [`ToolRuntime`] and applies capability, consequence,
/// and magnitude checks before delegating to the inner runtime.
pub struct RiskGatedToolRuntime {
    inner: Arc<dyn ToolRuntime>,
    /// The capability set — the MCP must be granted to execute.
    capabilities: CapabilitySet,
    /// Consequence catalog: `(mcp_name, consequence)` pairs for each MCP.
    consequence_catalog: Vec<(String, Consequence)>,
    /// Base directory for proposal files. Proposals are written to `proposals_dir/proposals/`.
    proposals_dir: PathBuf,
    /// The current user message / goal context used for magnitude assessment.
    goal_context: String,
    /// Correlation base for proposal naming (e.g. session id or dispatch id).
    correlation_base: String,
    /// Signs every downgraded proposal so the daemon can detect tampering before approving it.
    signer: ProposalSigner,
}

impl RiskGatedToolRuntime {
    /// Build a new risk-gated runtime.
    ///
    /// # Arguments
    ///
    /// * `inner` - The inner tool runtime (shared) to delegate to when all guards pass.
    /// * `capabilities` - The set of granted capabilities for capability checking.
    /// * `consequence_catalog` - `(mcp_name, consequence)` pairs for consequence gating.
    /// * `proposals_dir` - Directory under which `proposals/` subdirectory holds proposal files.
    /// * `goal_context` - The user message / goal context for magnitude assessment.
    /// * `correlation_base` - A unique base string for naming generated proposals.
    /// * `signer` - Signs every downgraded proposal (see [`Proposal::integrity`]'s doc comment).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        inner: Arc<dyn ToolRuntime>,
        capabilities: CapabilitySet,
        consequence_catalog: Vec<(String, Consequence)>,
        proposals_dir: PathBuf,
        goal_context: String,
        correlation_base: String,
        signer: ProposalSigner,
    ) -> Self {
        Self {
            inner,
            capabilities,
            consequence_catalog,
            proposals_dir,
            goal_context,
            correlation_base,
            signer,
        }
    }
}

#[async_trait]
impl ToolRuntime for RiskGatedToolRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        self.inner.catalog()
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        let span = tracing::info_span!(
            "risk_gate",
            tool = %call.name,
            mcp = %mcp_of(&call.name),
        );

        async {
            let mcp_name = mcp_of(&call.name).to_string();

            // 1. Capability check: is the MCP granted?
            if !self.capabilities.grants_mcp(&mcp_name) {
                tracing::warn!(
                    mcp = %mcp_name,
                    "tool call blocked: MCP not in capability set"
                );
                return Err(format!(
                    "not authorized: MCP '{}' is not in the granted capability set",
                    mcp_name
                ));
            }

            // 2. Consequence check: look up the MCP's consequence.
            let consequence = self
                .consequence_catalog
                .iter()
                .find(|(name, _)| name == &mcp_name)
                .map(|(_, c)| *c)
                .unwrap_or(Consequence::ReadOnly);

            // 3. If consequence >= Irreversible, downgrade to proposal.
            if consequence >= Consequence::Irreversible {
                tracing::warn!(
                    mcp = %mcp_name,
                    ?consequence,
                    "tool call downgraded to proposal: high-consequence MCP"
                );
                let proposal_path = self
                    .write_proposal(call, "High-consequence MCP — requires human approval")
                    .await?;
                return Ok(proposal_message(&proposal_path));
            }

            // 4. Magnitude check: sweeping destructive behavior in args or goal context.
            let args_text = call.arguments.to_string();
            let full_context = format!("{} {}", self.goal_context, call.name);
            if is_sweeping_destructive(&args_text) || is_sweeping_destructive(&full_context) {
                tracing::warn!(
                    mcp = %mcp_name,
                    "tool call downgraded to proposal: sweeping destructive action"
                );
                let proposal_path = self
                    .write_proposal(call, "Sweeping destructive action — requires human approval")
                    .await?;
                return Ok(proposal_message(&proposal_path));
            }

            // 5. All guards pass — delegate to inner runtime.
            tracing::debug!(mcp = %mcp_name, "tool call passed risk gates");
            self.inner.invoke(call).await
        }
        .instrument(span)
        .await
    }
}

impl RiskGatedToolRuntime {
    /// Write a proposal file and return its path — or `Err` if it genuinely couldn't be written.
    /// A downgrade's whole safety property is that a human gets to review a real file before the
    /// action runs; silently reporting success on a failed write would tell the model (and
    /// therefore the user) that something is queued for approval when nothing was actually saved,
    /// with no way for either to notice. So a write failure here is a real tool-level error, fed
    /// back in-band like any other (`ToolRuntime::invoke`'s own contract), not swallowed.
    async fn write_proposal(
        &self,
        call: &ToolInvocation,
        rationale: &str,
    ) -> Result<PathBuf, String> {
        let proposal_id = format!(
            "{}-{}",
            self.correlation_base,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        );

        // `proposals_dir` is the vault's proposals/ directory (see this module's doc comment) — the
        // daemon's react() routes changes here into the same approve/execute pipeline the
        // dispatcher's own pre-flight proposals use.
        let proposals_subdir = self.proposals_dir.join("proposals");
        let proposal_path = proposals_subdir.join(format!("{proposal_id}.md"));

        let mut proposal = Proposal::pending(
            &proposal_id,
            &self.correlation_base,
            "liberado-chat",
            ProposedAction::ToolCalls(vec![liberado_common::ToolCall {
                tool: call.name.clone(),
                args: call.arguments.clone(),
            }]),
            rationale,
        );
        self.signer.sign(&mut proposal);

        let note = proposal.to_note();

        // Create the proposals directory if it doesn't exist.
        if let Err(e) = tokio::fs::create_dir_all(&proposals_subdir).await {
            tracing::error!(
                path = %proposals_subdir.display(),
                error = %e,
                "failed to create proposals directory"
            );
            return Err(format!(
                "could not create the proposals directory at {}: {e} — the action was NOT executed \
                 and NO proposal was saved for approval; this needs a human to look at the vault/\
                 filesystem before retrying",
                proposals_subdir.display()
            ));
        }

        if let Err(e) = tokio::fs::write(&proposal_path, &note).await {
            tracing::error!(
                path = %proposal_path.display(),
                error = %e,
                "failed to write proposal file"
            );
            return Err(format!(
                "could not save the proposal file at {}: {e} — the action was NOT executed and NO \
                 proposal was saved for approval; this needs a human to look at the vault/filesystem \
                 before retrying",
                proposal_path.display()
            ));
        }

        tracing::info!(
            path = %proposal_path.display(),
            tool = %call.name,
            "proposal written"
        );
        Ok(proposal_path)
    }
}

/// The tool *result* returned for a downgraded high-consequence/sweeping call. Phrased so the model
/// relays it unambiguously: the action did NOT run and waits on human approval.
fn proposal_message(path: &std::path::Path) -> String {
    format!(
        "PROPOSAL CREATED — the requested action was NOT executed. It is high-consequence and needs \
         your approval. Proposal saved at {}. It will run only after you approve it.",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_common::Capability;
    use liberado_provider::ToolDef;

    /// A mock inner runtime that returns a canned result.
    struct MockInner {
        tools: Vec<ToolDef>,
        invoked: std::sync::Mutex<Vec<ToolInvocation>>,
        result: Result<String, String>,
    }

    impl MockInner {
        fn new(tool_names: &[&str], result: Result<String, String>) -> Self {
            let tools = tool_names
                .iter()
                .map(|n| ToolDef::new(*n, "test tool", serde_json::json!({ "type": "object" })))
                .collect();
            Self {
                tools,
                invoked: std::sync::Mutex::new(Vec::new()),
                result,
            }
        }
    }

    #[async_trait]
    impl ToolRuntime for MockInner {
        fn catalog(&self) -> Vec<ToolDef> {
            self.tools.clone()
        }

        async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
            self.invoked.lock().unwrap().push(call.clone());
            self.result.clone()
        }
    }

    fn test_runtime(
        inner: impl ToolRuntime + 'static,
        capabilities: CapabilitySet,
        consequence_catalog: &[(&str, Consequence)],
    ) -> RiskGatedToolRuntime {
        let catalog: Vec<(String, Consequence)> = consequence_catalog
            .iter()
            .map(|(n, c)| (n.to_string(), *c))
            .collect();

        RiskGatedToolRuntime::new(
            Arc::new(inner),
            capabilities,
            catalog,
            std::env::temp_dir(),
            "test goal".into(),
            "test-correlation".into(),
            ProposalSigner::random(),
        )
    }

    #[tokio::test]
    async fn low_consequence_call_passes_through() {
        let inner = MockInner::new(&["my-mcp:read"], Ok("data".into()));
        let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("my-mcp".into())]);
        let rt = test_runtime(inner, caps, &[("my-mcp", Consequence::ReadOnly)]);

        let call = ToolInvocation::new("c1", "my-mcp:read", serde_json::json!({}));
        let result = rt.invoke(&call).await;
        assert_eq!(result, Ok("data".into()));
    }

    #[tokio::test]
    async fn high_consequence_call_is_downgraded_to_proposal() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(MockInner::new(&["email-mcp:send"], Ok("sent".into())));
        let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("email-mcp".into())]);
        let signer = ProposalSigner::random();
        let rt = RiskGatedToolRuntime::new(
            inner.clone(),
            caps,
            vec![("email-mcp".into(), Consequence::External)],
            dir.path().to_path_buf(),
            "send an email".into(),
            "test-email".into(),
            signer.clone(),
        );

        let call = ToolInvocation::new("c1", "email-mcp:send", serde_json::json!({"to": "boss"}));
        let result = rt.invoke(&call).await;
        // A downgrade is a tool *result* (Ok), not an error — so the model relays it cleanly.
        let msg = result.expect("downgrade should be an Ok tool result, not an Err");
        assert!(
            msg.contains("PROPOSAL CREATED") && msg.contains("NOT executed"),
            "message must state the action did not run: {msg}"
        );

        // The inner tool must NOT have run.
        assert!(
            inner.invoked.lock().unwrap().is_empty(),
            "high-consequence call must not invoke the inner tool"
        );

        // Verify the proposal file was written, and it's signed with the runtime's own signer.
        let proposals_dir = dir.path().join("proposals");
        let mut entries = tokio::fs::read_dir(&proposals_dir).await.unwrap();
        let entry = entries.next_entry().await.unwrap().expect("proposal file should exist");
        let content = tokio::fs::read_to_string(entry.path()).await.unwrap();
        let written = liberado_common::Proposal::from_note(&content).unwrap();
        assert!(
            signer.verify(&written),
            "the written proposal must verify against the runtime's own signer"
        );
    }

    #[tokio::test]
    async fn out_of_capability_call_is_rejected() {
        let inner = MockInner::new(&["email-mcp:send"], Ok("sent".into()));
        let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("tasks-mcp".into())]); // email not granted
        let rt = test_runtime(inner, caps, &[("email-mcp", Consequence::Reversible)]);

        let call = ToolInvocation::new("c1", "email-mcp:send", serde_json::json!({}));
        let result = rt.invoke(&call).await;
        assert!(result.is_err(), "ungranted call should be rejected");
        assert!(result.unwrap_err().contains("not authorized"));
    }

    #[tokio::test]
    async fn catalog_delegates_to_inner() {
        let inner = MockInner::new(&["my-mcp:read", "my-mcp:write"], Ok("ok".into()));
        let rt = test_runtime(
            inner,
            CapabilitySet::empty(),
            &[("my-mcp", Consequence::ReadOnly)],
        );

        let catalog = rt.catalog();
        assert_eq!(catalog.len(), 2);
        let names: Vec<&str> = catalog.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"my-mcp:read"));
        assert!(names.contains(&"my-mcp:write"));
    }

    #[tokio::test]
    async fn a_proposal_write_failure_is_a_real_error_not_a_silent_ok() {
        // proposals_dir points at a path whose parent component is an existing *file*, so
        // create_dir_all(proposals_dir/"proposals") cannot succeed — this must surface as a real
        // Err, not a fabricated "PROPOSAL CREATED" success with nothing actually written.
        let dir = tempfile::TempDir::new().unwrap();
        let occupied_by_a_file = dir.path().join("occupied");
        tokio::fs::write(&occupied_by_a_file, b"not a directory").await.unwrap();

        let inner = Arc::new(MockInner::new(&["email-mcp:send"], Ok("sent".into())));
        let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("email-mcp".into())]);
        let rt = RiskGatedToolRuntime::new(
            inner.clone(),
            caps,
            vec![("email-mcp".into(), Consequence::External)],
            occupied_by_a_file,
            "send an email".into(),
            "test-write-failure".into(),
            ProposalSigner::random(),
        );

        let call = ToolInvocation::new("c1", "email-mcp:send", serde_json::json!({"to": "boss"}));
        let result = rt.invoke(&call).await;
        assert!(
            result.is_err(),
            "a genuine write failure must surface as Err, not a fabricated success message"
        );
        assert!(
            !result.unwrap_err().contains("PROPOSAL CREATED"),
            "must not claim a proposal was created when nothing was written"
        );
        assert!(
            inner.invoked.lock().unwrap().is_empty(),
            "the inner tool must still never run, regardless of whether the proposal was saved"
        );
    }

    #[tokio::test]
    async fn sweeping_destructive_call_is_downgraded() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(MockInner::new(&["vault-mcp:delete"], Ok("done".into())));
        let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("vault-mcp".into())]);
        let rt = RiskGatedToolRuntime::new(
            inner.clone(),
            caps,
            vec![("vault-mcp".into(), Consequence::Reversible)], // Low consequence
            dir.path().to_path_buf(),
            "delete all notes".into(), // Sweeping+destructive goal
            "test-sweep".into(),
            ProposalSigner::random(),
        );

        let call = ToolInvocation::new("c1", "vault-mcp:delete", serde_json::json!({"path": "all"}));
        let result = rt.invoke(&call).await;
        // A downgrade is a tool *result* (Ok), not an error.
        let msg = result.expect("downgrade should be an Ok tool result, not an Err");
        assert!(msg.contains("PROPOSAL CREATED") && msg.contains("NOT executed"));
        // The inner tool must NOT have run.
        assert!(
            inner.invoked.lock().unwrap().is_empty(),
            "sweeping-destructive call must not invoke the inner tool"
        );
    }
}
