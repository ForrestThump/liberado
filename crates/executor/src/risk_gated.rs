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
//!
//! ## Keeping this in sync with the dispatcher's pre-flight guard (`liberado-dispatcher/src/guards.rs`)
//!
//! The zone-write-class check is unified (`liberado_common::zone_write_restriction`) so it cannot
//! drift between the two enforcement points. The capability and consequence checks below are NOT
//! unified — they operate over different shapes for good reason (this runtime checks one live
//! call; the dispatcher's guard checks a decision's declared seed calls before anything runs) — but
//! if you add a **new** guard here, check whether `guards.rs::evaluate` needs the equivalent, and
//! vice versa. That sequencing risk is the reason this note exists.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use liberado_common::{
    Capability, CapabilitySet, Consequence, McpDescriptor, Proposal, ProposalSigner,
    ProposedAction, WriteClass, WriteTarget, Zone, bare_tool_name, is_sweeping_destructive, mcp_of,
    write_target,
};
use liberado_notify::Notifier;
use liberado_provider::{ToolDef, ToolInvocation};
use tracing::Instrument;

use crate::ToolRuntime;

/// A runtime guard that wraps an inner [`ToolRuntime`] and applies capability, consequence,
/// zone-write-class, and magnitude checks before delegating to the inner runtime.
pub struct RiskGatedToolRuntime {
    inner: Arc<dyn ToolRuntime>,
    /// The capability set — the MCP must be granted to execute.
    capabilities: CapabilitySet,
    /// Consequence catalog: `(mcp_name, consequence)` pairs for each MCP.
    consequence_catalog: Vec<(String, Consequence)>,
    /// Per-MCP zone declarations (§6 #2's zone-write-class guard) — the same `McpDescriptor` list
    /// the live `CapabilityCatalog` already provides (`catalog.descriptors()`), reused directly
    /// rather than deriving yet another shape the way `consequence_catalog` is its own tuple list.
    zone_catalog: Vec<McpDescriptor>,
    /// `(zone, write_class)` pairs from `Policy.zones` — what a call's resolved target zone (via
    /// `zone_catalog`) is checked against. A zone absent here fails safe to
    /// `WriteClass::default()` (`ProposalOnly`), the same conservative default
    /// `Policy::write_class` itself uses for an unlisted zone.
    zone_write_classes: Vec<(String, WriteClass)>,
    /// Base directory for proposal files. Proposals are written to `proposals_dir/proposals/`.
    proposals_dir: PathBuf,
    /// The current user message / goal context used for magnitude assessment.
    goal_context: String,
    /// Correlation base for proposal naming (e.g. session id or dispatch id).
    correlation_base: String,
    /// Signs every downgraded proposal so the daemon can detect tampering before approving it.
    signer: ProposalSigner,
    /// Which named dispatcher/executor pool (Decision 18 checkpoint #3) this runtime's owning
    /// `Orchestrator` *is* — stamped onto every proposal this runtime downgrades to, so approval
    /// later executes it via this same pool's authority, never a different one.
    pool_name: String,
    /// Told about every proposal this runtime writes — optional (`None` by default via
    /// [`with_notifier`](Self::with_notifier), added as a builder step rather than a `new()`
    /// parameter so existing call sites don't need to change). Best-effort: a notification
    /// failure never blocks or fails the write it's reporting on.
    notifier: Option<Arc<dyn Notifier>>,
}

impl RiskGatedToolRuntime {
    /// Build a new risk-gated runtime.
    ///
    /// # Arguments
    ///
    /// * `inner` - The inner tool runtime (shared) to delegate to when all guards pass.
    /// * `capabilities` - The set of granted capabilities for capability checking.
    /// * `consequence_catalog` - `(mcp_name, consequence)` pairs for consequence gating.
    /// * `zone_catalog` - The MCP descriptors (zone declarations) for the zone-write-class guard.
    /// * `zone_write_classes` - `(zone, write_class)` pairs from `Policy.zones`.
    /// * `proposals_dir` - Directory under which `proposals/` subdirectory holds proposal files.
    /// * `goal_context` - The user message / goal context for magnitude assessment.
    /// * `correlation_base` - A unique base string for naming generated proposals.
    /// * `signer` - Signs every downgraded proposal (see [`Proposal::integrity`]'s doc comment).
    /// * `pool_name` - The owning `Orchestrator`'s pool name, stamped onto every proposal built here.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        inner: Arc<dyn ToolRuntime>,
        capabilities: CapabilitySet,
        consequence_catalog: Vec<(String, Consequence)>,
        zone_catalog: Vec<McpDescriptor>,
        zone_write_classes: Vec<(String, WriteClass)>,
        proposals_dir: PathBuf,
        goal_context: String,
        correlation_base: String,
        signer: ProposalSigner,
        pool_name: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            capabilities,
            consequence_catalog,
            zone_catalog,
            zone_write_classes,
            proposals_dir,
            goal_context,
            correlation_base,
            signer,
            pool_name: pool_name.into(),
            notifier: None,
        }
    }

    /// Attach a [`Notifier`] to tell about every proposal this runtime writes. Optional — a
    /// runtime with no notifier attached just never sends anything, the same as today.
    pub fn with_notifier(mut self, notifier: Arc<dyn Notifier>) -> Self {
        self.notifier = Some(notifier);
        self
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

            // 2. Consequence check: look up the MCP's consequence. A miss here means the
            // capability set (checked above) and consequence_catalog have drifted — the MCP is
            // granted but undescribed, most likely a name mismatch between the two catalogs.
            // Fails open to ReadOnly (matching the dispatcher pre-flight guard's own documented
            // "undescribed contributes nothing" stance — see guards.rs's max_consequence), but
            // logs so a catalog typo isn't a silent risk downgrade.
            let consequence = match self
                .consequence_catalog
                .iter()
                .find(|(name, _)| name == &mcp_name)
            {
                Some((_, c)) => *c,
                None => {
                    tracing::warn!(
                        mcp = %mcp_name,
                        "MCP is capability-granted but missing from consequence_catalog — \
                         defaulting to ReadOnly; check for a name mismatch between the two catalogs"
                    );
                    Consequence::ReadOnly
                }
            };

            // 2b. **What does this call write, and may this grant write it? (F1)**
            //
            // `ExecuteMcp` says you may *call* this MCP. It does not say you may *write* with it —
            // and until 2026-07-14 nothing else said so either: `Capability::Write(Zone)` was never
            // consulted at this boundary, so a grant of `Read` + `ExecuteMcp("turbovault")` could
            // write the entire vault. A live dispatch session with no `Write` capability wrote a
            // note its profile explicitly withheld.
            //
            // The zone is resolved ONCE here and reused by the write-class check below. That
            // matters: both guards were previously inert for the same reason (no MCP declared a
            // zone, so nothing resolved), and resolving in one place means they cannot drift back
            // apart. For a path-addressed MCP the zone depends on the call's *arguments*, so this
            // is the only place it can be known.
            let write_target = self
                .zone_catalog
                .iter()
                .find(|d| d.name == mcp_name)
                .map(|d| write_target(d, bare_tool_name(&call.name), &call.arguments))
                .unwrap_or(WriteTarget::NotAWrite);

            let write_zone = match &write_target {
                // A write we cannot place. Fail closed — refusing a write whose target is unknown
                // is the only safe answer, and it is a config bug worth surfacing loudly.
                WriteTarget::Undeterminable(why) => {
                    tracing::warn!(mcp = %mcp_name, %why, "tool call refused: undeterminable write zone");
                    return Err(format!("not authorized: {why}"));
                }
                WriteTarget::Zone(zone) => Some(zone.clone()),
                WriteTarget::NotAWrite => None,
            };

            if let Some(zone) = &write_zone {
                // Deliberately a **refusal**, not a proposal downgrade. The guards below ask "this
                // is permitted, but is it risky enough to need a human?" — a question that only
                // makes sense once "is this permitted at all?" is yes. A missing capability is an
                // authority failure, and reads like one: same shape as the `grants_mcp` refusal.
                if !self.capabilities.contains(&Capability::Write(Zone::vault(zone))) {
                    tracing::warn!(
                        mcp = %mcp_name,
                        tool = %bare_tool_name(&call.name),
                        %zone,
                        "tool call refused: no Write capability for the zone it targets"
                    );
                    return Err(format!(
                        "not authorized: '{}' writes to zone '{zone}', and this session's grant \
                         does not include Write({zone}). Calling an MCP is not permission to write \
                         with it.",
                        call.name
                    ));
                }
            }

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

            // 3b. Zone-write-class check (§6 #2). Now driven by the SAME `write_zone` resolved in
            // 2b rather than re-deriving from the tool name alone — which is what made it inert for
            // a path-addressed MCP: `resolve_zone` could not see the `path` argument, returned
            // `None`, and a write to the `human_only` finance zone sailed straight through. The two
            // guards ask different questions of one answer: 2b asks *may you*, this asks *is it
            // safe to do directly*.
            let bare_tool = bare_tool_name(&call.name);
            let restricted_zone = write_zone.as_ref().filter(|zone| {
                let class = self
                    .zone_write_classes
                    .iter()
                    .find(|(z, _)| z == *zone)
                    // An undeclared zone fails safe to the restrictive default, same as
                    // `zone_write_restriction` does — an unknown zone is not a licence.
                    .map_or(WriteClass::default(), |(_, wc)| *wc);
                !class.allows_direct_agent_write()
            });
            if let Some(zone) = restricted_zone {
                tracing::warn!(
                    mcp = %mcp_name,
                    tool = %bare_tool,
                    %zone,
                    "tool call downgraded to proposal: zone write-class restricted"
                );
                let proposal_path = self
                    .write_proposal(
                        call,
                        &format!("Write targets the '{zone}' zone, which requires human approval"),
                    )
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
                    .write_proposal(
                        call,
                        "Sweeping destructive action — requires human approval",
                    )
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
        let proposals_subdir = self.proposals_dir.join(liberado_common::PROPOSALS_DIR);
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
        proposal.pool = Some(self.pool_name.clone());
        let proposal = self.signer.sign(proposal);

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

        if let Some(notifier) = &self.notifier {
            let message = format!(
                "Liberado: a new proposal needs your review.\n{rationale}\nTool: {}\nSaved at: {}",
                call.name,
                proposal_path.display()
            );
            if let Err(e) = notifier.notify_proposal(&proposal_id, &message).await {
                // Best-effort — the proposal itself is already safely written; a failed
                // notification just means the human finds out by checking the vault instead of
                // their phone, not that the safety property (a human gets to review it) broke.
                tracing::warn!(error = %e, "failed to send proposal notification");
            }
        }

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
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            "test goal".into(),
            "test-correlation".into(),
            ProposalSigner::random(),
            "default",
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
            Vec::new(),
            Vec::new(),
            dir.path().to_path_buf(),
            "send an email".into(),
            "test-email".into(),
            signer.clone(),
            "default",
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
        let entry = entries
            .next_entry()
            .await
            .unwrap()
            .expect("proposal file should exist");
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
        tokio::fs::write(&occupied_by_a_file, b"not a directory")
            .await
            .unwrap();

        let inner = Arc::new(MockInner::new(&["email-mcp:send"], Ok("sent".into())));
        let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("email-mcp".into())]);
        let rt = RiskGatedToolRuntime::new(
            inner.clone(),
            caps,
            vec![("email-mcp".into(), Consequence::External)],
            Vec::new(),
            Vec::new(),
            occupied_by_a_file,
            "send an email".into(),
            "test-write-failure".into(),
            ProposalSigner::random(),
            "default",
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
            Vec::new(),
            Vec::new(),
            dir.path().to_path_buf(),
            "delete all notes".into(), // Sweeping+destructive goal
            "test-sweep".into(),
            ProposalSigner::random(),
            "default",
        );

        let call =
            ToolInvocation::new("c1", "vault-mcp:delete", serde_json::json!({"path": "all"}));
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

    fn vault_descriptor() -> McpDescriptor {
        McpDescriptor {
            name: "vault".into(),
            description: "git-tracked vault".into(),
            consequence: Consequence::Reversible, // low, so this isolates the zone check
            provenance: None,
            default_zone: Some("tasks".into()),
            tool_zones: vec![("write_review".into(), Some("reviews".into()))],
            zone_from_arg: None,
            write_tools: Vec::new(),
        }
    }

    #[tokio::test]
    async fn zone_restricted_call_is_downgraded_to_proposal() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(MockInner::new(&["vault:write_review"], Ok("wrote".into())));
        // Holds the authority to write `reviews` — this test is about whether the write is SAFE to
        // do directly (write-class), which is a question that only arises once it is PERMITTED.
        let caps = CapabilitySet::from_iter([
            Capability::ExecuteMcp("vault".into()),
            Capability::Write(Zone::vault("reviews")),
        ]);
        let rt = RiskGatedToolRuntime::new(
            inner.clone(),
            caps,
            vec![("vault".into(), Consequence::Reversible)],
            vec![vault_descriptor()],
            vec![("reviews".to_string(), WriteClass::ProposalOnly)],
            dir.path().to_path_buf(),
            "write a review note".into(),
            "test-zone".into(),
            ProposalSigner::random(),
            "default",
        );

        let call = ToolInvocation::new(
            "c1",
            "vault:write_review",
            serde_json::json!({"content": "..."}),
        );
        let result = rt.invoke(&call).await;
        let msg = result.expect("downgrade should be an Ok tool result, not an Err");
        assert!(msg.contains("PROPOSAL CREATED") && msg.contains("NOT executed"));
        assert!(
            inner.invoked.lock().unwrap().is_empty(),
            "a zone-restricted call must not invoke the inner tool"
        );
    }

    #[tokio::test]
    async fn zone_agent_writable_call_passes_through() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(MockInner::new(&["vault:write_review"], Ok("wrote".into())));
        let caps = CapabilitySet::from_iter([
            Capability::ExecuteMcp("vault".into()),
            Capability::Write(Zone::vault("reviews")),
        ]);
        let rt = RiskGatedToolRuntime::new(
            inner.clone(),
            caps,
            vec![("vault".into(), Consequence::Reversible)],
            vec![vault_descriptor()],
            vec![("reviews".to_string(), WriteClass::AgentWritable)],
            dir.path().to_path_buf(),
            "write a review note".into(),
            "test-zone-ok".into(),
            ProposalSigner::random(),
            "default",
        );

        let call = ToolInvocation::new(
            "c1",
            "vault:write_review",
            serde_json::json!({"content": "..."}),
        );
        let result = rt.invoke(&call).await;
        assert_eq!(result, Ok("wrote".into()));
        assert_eq!(inner.invoked.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn call_to_an_mcp_not_in_the_zone_catalog_is_unaffected() {
        // Backward-compat case: an empty zone_catalog (as every pre-existing test in this file
        // uses) must never trip the zone-write-class check, regardless of zone_write_classes.
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(MockInner::new(&["vault:write_review"], Ok("wrote".into())));
        let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("vault".into())]);
        let rt = RiskGatedToolRuntime::new(
            inner.clone(),
            caps,
            vec![("vault".into(), Consequence::Reversible)],
            Vec::new(), // no zone declarations at all for "vault"
            vec![("reviews".to_string(), WriteClass::ProposalOnly)],
            dir.path().to_path_buf(),
            "write a review note".into(),
            "test-zone-untracked".into(),
            ProposalSigner::random(),
            "default",
        );

        let call = ToolInvocation::new(
            "c1",
            "vault:write_review",
            serde_json::json!({"content": "..."}),
        );
        let result = rt.invoke(&call).await;
        assert_eq!(result, Ok("wrote".into()));
    }

    #[tokio::test]
    #[ignore = "requires LIBERADO_TELEGRAM_BOT_TOKEN + LIBERADO_TELEGRAM_CHAT_ID + network access"]
    async fn live_high_consequence_downgrade_sends_a_real_telegram_notification() {
        // Full-integration live check: a real proposal write through the actual production guard
        // path, with a real Notifier attached, not just liberado-notify's own bare TelegramNotifier
        // in isolation — proves `with_notifier`/the `invoke`-path notify call are wired correctly,
        // not just that the underlying HTTP call works.
        let notifier = liberado_notify::TelegramNotifier::from_env().expect(
            "set LIBERADO_TELEGRAM_BOT_TOKEN and LIBERADO_TELEGRAM_CHAT_ID to run this test",
        );
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(MockInner::new(&["email-mcp:send"], Ok("sent".into())));
        let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("email-mcp".into())]);
        let rt = RiskGatedToolRuntime::new(
            inner,
            caps,
            vec![("email-mcp".into(), Consequence::External)],
            Vec::new(),
            Vec::new(),
            dir.path().to_path_buf(),
            "send an email".into(),
            "live-notify-test".into(),
            ProposalSigner::random(),
            "default",
        )
        .with_notifier(Arc::new(notifier));

        let call = ToolInvocation::new("c1", "email-mcp:send", serde_json::json!({"to": "boss"}));
        let result = rt.invoke(&call).await;
        assert!(
            result
                .expect("downgrade should be Ok")
                .contains("PROPOSAL CREATED")
        );
    }

    /// F1, the live failure, pinned: a grant that may CALL an MCP but holds no Write for the zone it
    /// targets must be refused. Before 2026-07-14 this call succeeded — `Capability::Write` was never
    /// consulted here, so `ExecuteMcp("turbovault")` was in effect "write the whole vault".
    #[tokio::test]
    async fn calling_an_mcp_is_not_permission_to_write_with_it() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(MockInner::new(&["vault:write_review"], Ok("wrote".into())));
        // ExecuteMcp but NO Write — exactly the dispatch-readonly profile from the live control.
        let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("vault".into())]);
        let rt = RiskGatedToolRuntime::new(
            inner.clone(),
            caps,
            vec![("vault".into(), Consequence::Reversible)],
            vec![vault_descriptor()],
            // `reviews` is freely agent-writable: the RISK gate would happily pass this. The refusal
            // must come from AUTHORITY, which is a different question and is asked first.
            vec![("reviews".to_string(), WriteClass::AgentWritable)],
            dir.path().to_path_buf(),
            "write a review note".into(),
            "test-f1".into(),
            ProposalSigner::random(),
            "default",
        );

        let call = ToolInvocation::new("c1", "vault:write_review", serde_json::json!({"c": "..."}));
        let err = rt
            .invoke(&call)
            .await
            .expect_err("a write with no Write capability must be REFUSED, not downgraded");
        assert!(err.contains("not authorized"), "{err}");
        assert!(
            err.contains("reviews"),
            "must name the zone it refused: {err}"
        );
        assert!(
            inner.invoked.lock().unwrap().is_empty(),
            "and the tool must never have run"
        );
    }

    /// The path-addressed case, which is what TurboVault actually is: the zone comes from the call's
    /// arguments, so `Write(tasks)` must NOT authorize a write to `decisions/`.
    #[tokio::test]
    async fn a_path_addressed_write_is_checked_against_the_zone_the_path_names() {
        let dir = tempfile::TempDir::new().unwrap();
        let descriptor = McpDescriptor {
            name: "turbovault".into(),
            description: "path-addressed vault".into(),
            consequence: Consequence::Reversible,
            provenance: None,
            default_zone: None,
            tool_zones: Vec::new(),
            zone_from_arg: Some("path".into()),
            write_tools: vec!["write_note".into()],
        };
        let caps = CapabilitySet::from_iter([
            Capability::ExecuteMcp("turbovault".into()),
            Capability::Write(Zone::vault("tasks")),
        ]);
        let classes = vec![
            ("tasks".to_string(), WriteClass::AgentWritable),
            ("decisions".to_string(), WriteClass::AgentWritable),
        ];
        let make = |inner: Arc<MockInner>| {
            RiskGatedToolRuntime::new(
                inner,
                caps.clone(),
                vec![("turbovault".into(), Consequence::Reversible)],
                vec![descriptor.clone()],
                classes.clone(),
                dir.path().to_path_buf(),
                "write a note".into(),
                "test-path".into(),
                ProposalSigner::random(),
                "default",
            )
        };

        // In-zone: permitted.
        let ok_inner = Arc::new(MockInner::new(
            &["turbovault:write_note"],
            Ok("wrote".into()),
        ));
        let ok = make(ok_inner.clone())
            .invoke(&ToolInvocation::new(
                "c1",
                "turbovault:write_note",
                serde_json::json!({"path": "tasks/a.md"}),
            ))
            .await;
        assert_eq!(ok, Ok("wrote".into()));

        // Out of zone: the SAME tool, the SAME grant — refused, because the path names a zone this
        // grant cannot write. A fixed `default_zone` could never have caught this.
        let bad_inner = Arc::new(MockInner::new(
            &["turbovault:write_note"],
            Ok("wrote".into()),
        ));
        let err = make(bad_inner.clone())
            .invoke(&ToolInvocation::new(
                "c2",
                "turbovault:write_note",
                serde_json::json!({"path": "decisions/b.md"}),
            ))
            .await
            .expect_err("Write(tasks) must not authorize a write to decisions/");
        assert!(err.contains("decisions"), "{err}");
        assert!(bad_inner.invoked.lock().unwrap().is_empty());
    }
}
