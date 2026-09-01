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

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use liberado_common::{
    ApprovedGuard, Capability, CapabilityCatalog, CapabilitySet, Consequence, McpDescriptor,
    Proposal, ProposalSigner, ProposedAction, RiskWaiverSet, SignedProposal, WriteClass,
    WriteTarget, Zone, bare_tool_name, mcp_of, write_target,
};
use liberado_notify::Notifier;
use liberado_provider::{ToolDef, ToolInvocation};
use tracing::Instrument;

use crate::ToolRuntime;

#[path = "risk_gated_skip.rs"]
mod skip;

/// A runtime guard that wraps an inner [`ToolRuntime`] and applies capability, consequence,
/// zone-write-class, and magnitude checks before delegating to the inner runtime.
pub struct RiskGatedToolRuntime {
    inner: Arc<dyn ToolRuntime>,
    /// The capability set — the MCP must be granted to execute.
    capabilities: CapabilitySet,
    /// Consequence catalog: `(mcp_name, consequence)` pairs for each MCP.
    /// Used when [`live_catalog`](Self::with_live_catalog) is not set (tests / static fixtures).
    consequence_catalog: Vec<(String, Consequence)>,
    /// Per-MCP zone declarations (§6 #2's zone-write-class guard).
    /// Used when `live_catalog` is not set.
    zone_catalog: Vec<McpDescriptor>,
    /// When set, consequence and zone declarations are read **live** on every invoke so hot-reload
    /// of topology MCP peers is reflected without rebuilding this gate.
    live_catalog: Option<Arc<CapabilityCatalog>>,
    /// `(zone, write_class)` pairs from `Policy.zones` — what a call's resolved target zone (via
    /// `zone_catalog`) is checked against. A zone absent here fails safe to
    /// `WriteClass::default()` (`ProposalOnly`), the same conservative default
    /// `Policy::write_class` itself uses for an unlisted zone.
    zone_write_classes: Vec<(String, WriteClass)>,
    /// Declarative risk waivers loaded from `policy.toml`. A waiver matching the call's
    /// (mcp, tool, zone) suppresses the magnitude guard for this call. Does not affect any
    /// other guard; the capability and consequence checks still gate normally.
    risk_waivers: RiskWaiverSet,
    /// One guard already authorized by the signed proposal that started this adaptive run.
    /// Capability and target resolution are never bypassed, and every other risk guard remains on.
    approved_guard: Option<ApprovedGuard>,
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
    /// Set to `true` the moment this runtime raises a proposal / permission-request **and**
    /// successfully surfaces it to the human out-of-band (an interactive notification went out). A
    /// shared handle so the owning `Orchestrator` can read it back after the run and stamp it onto
    /// the `Report` (`Report::deferred_to_human`), which a chat surface uses to drop a redundant
    /// "you need to grant permission" reply. Stays `false` when there's no notifier or the notify
    /// failed — then the chat reply is the only signal and must NOT be suppressed. Defaults to a
    /// private, unshared flag (a runtime nobody wired one into simply never reports a deferral).
    notified_deferral: Arc<AtomicBool>,
    /// Set `true` by tests to simulate `create_dir_all` failing on the next proposal downgrade.
    pub fail_next_create_dir: Arc<AtomicBool>,
    /// Set `true` by tests to simulate `write` failing on the next proposal downgrade.
    pub fail_next_write: Arc<AtomicBool>,
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
    /// * `risk_waivers` - Risk waivers from `policy.toml`. Empty by default — the magnitude heuristic
    ///   fires as it did before this feature shipped.
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
            live_catalog: None,
            zone_write_classes,
            proposals_dir,
            goal_context,
            correlation_base,
            signer,
            pool_name: pool_name.into(),
            notifier: None,
            notified_deferral: Arc::new(AtomicBool::new(false)),
            fail_next_create_dir: Arc::new(AtomicBool::new(false)),
            fail_next_write: Arc::new(AtomicBool::new(false)),
            risk_waivers: RiskWaiverSet::empty(),
            approved_guard: None,
        }
    }

    /// Set the risk-waiver set consulted by the magnitude guard. Empty by default.
    pub fn with_risk_waivers(mut self, waivers: RiskWaiverSet) -> Self {
        self.risk_waivers = waivers;
        self
    }

    /// Mark one risk guard as already approved for this signed adaptive-goal execution.
    pub fn with_approved_guard(mut self, guard: ApprovedGuard) -> Self {
        self.approved_guard = Some(guard);
        self
    }

    /// Prefer the live capability catalog for consequence + zone resolution on every invoke
    /// (hot-reload safe). Snapshot vectors passed to [`new`](Self::new) remain as fallback when
    /// this is unset.
    pub fn with_live_catalog(mut self, catalog: Arc<CapabilityCatalog>) -> Self {
        self.live_catalog = Some(catalog);
        self
    }

    /// Attach a [`Notifier`] to tell about every proposal this runtime writes. Optional — a
    /// runtime with no notifier attached just never sends anything, the same as today.
    pub fn with_notifier(mut self, notifier: Arc<dyn Notifier>) -> Self {
        self.notifier = Some(notifier);
        self
    }

    /// Emit the one line that says **which guard decided, and what would change its mind**.
    ///
    /// Every guard here can say no, and until this existed none of them could say *"it was me"*.
    /// That is the specific reason this layer is hard to operate: a refused write and a
    /// deliberately-protected zone produce the identical observable — a proposal — so a
    /// misconfiguration, a missing grant, and a working policy all look the same from outside. A
    /// capability bug that denied every subagent write survived months of use behind that
    /// ambiguity, while the daemon simultaneously logged that the grant was present.
    ///
    /// `needed` and `held` are the fields that actually shorten a debugging session: they turn
    /// "why was this blocked" into a diff you can read. Kept to one event with stable field names
    /// so it greps cleanly (`guard=`, `verdict=`) and can back a metric later.
    fn authority_decision(
        &self,
        guard: &'static str,
        verdict: &'static str,
        call: &ToolInvocation,
        zone: Option<&str>,
        needed: &str,
    ) {
        tracing::warn!(
            guard,
            verdict,
            mcp = %mcp_of(&call.name),
            tool = %bare_tool_name(&call.name),
            zone = zone.unwrap_or("-"),
            needed = %needed,
            held = %held_summary(&self.capabilities),
            "authority decision"
        );
    }

    fn consequence_of(&self, mcp_name: &str) -> Consequence {
        if let Some(cat) = &self.live_catalog {
            return cat.get(mcp_name).map(|d| d.consequence).unwrap_or_else(|| {
                tracing::warn!(
                    mcp = %mcp_name,
                    "MCP is capability-granted but missing from live catalog — \
                     defaulting to ReadOnly"
                );
                Consequence::ReadOnly
            });
        }
        match self
            .consequence_catalog
            .iter()
            .find(|(name, _)| name == mcp_name)
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
        }
    }

    /// The declaration this call is judged against — the live catalog when one is attached, else
    /// the boot-time snapshot. Shared by the zone-write guard and the magnitude guard so both read
    /// the same declaration for one call.
    fn descriptor_of(&self, mcp_name: &str) -> Option<McpDescriptor> {
        if let Some(cat) = &self.live_catalog {
            cat.get(mcp_name)
        } else {
            self.zone_catalog
                .iter()
                .find(|d| d.name == mcp_name)
                .cloned()
        }
    }

    fn write_target_of(&self, mcp_name: &str, call: &ToolInvocation) -> WriteTarget {
        self.descriptor_of(mcp_name)
            .map(|d| write_target(&d, bare_tool_name(&call.name), &call.arguments))
            .unwrap_or(WriteTarget::NotAWrite)
    }

    /// Share the flag this runtime raises when it defers a call to the human out-of-band, so the
    /// owning `Orchestrator` can read it back after the run (see `notified_deferral` and
    /// [`took_deferral_to_human`](Self::took_deferral_to_human)). Without this, the runtime still
    /// tracks the flag on its own private handle — it just has no one to report it to.
    pub fn with_deferral_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.notified_deferral = flag;
        self
    }

    /// Whether this runtime raised a proposal / permission-request during its run **and** surfaced
    /// it to the human out-of-band. See `notified_deferral`.
    pub fn took_deferral_to_human(&self) -> bool {
        self.notified_deferral.load(Ordering::Relaxed)
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

            // 1. Capability check: is **this tool** granted?
            //
            // `grants_tool`, not `grants_mcp`: a grant may name the whole server
            // (`ExecuteMcp("turbovault")`) or one tool on it (`ExecuteTool("turbovault:read_note")`),
            // and only the tool-level question distinguishes them. Asking `grants_mcp` here would
            // pass every tool on a server the grant only meant to open a crack of — precisely what
            // per-tool grants exist to prevent, and it would have done so silently.
            if !self.capabilities.grants_tool(&call.name) {
                self.authority_decision(
                    "mcp_grant",
                    "refused",
                    call,
                    None,
                    &format!("ExecuteTool(\"{}\")", call.name),
                );
                return Err(format!(
                    "not authorized: tool '{}' is not in the granted capability set",
                    call.name
                ));
            }

            // 2. Consequence check — live catalog when wired (hot-reload), else boot snapshot.
            let consequence = self.consequence_of(&mcp_name);

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
            let write_target = self.write_target_of(&mcp_name, call);

            let write_zone = match &write_target {
                // A write we cannot place. Fail closed — refusing a write whose target is unknown
                // is the only safe answer, and it is a config bug worth surfacing loudly.
                WriteTarget::Undeterminable(why) => {
                    self.authority_decision(
                        "write_zone_resolution",
                        "refused",
                        call,
                        None,
                        "a resolvable target zone (declare zone_from_arg/write_tools, or pass a \
                         zone-qualified path)",
                    );
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
                    self.authority_decision(
                        "write_capability",
                        if self.notifier.is_some() { "permission_request" } else { "refused" },
                        call,
                        Some(zone),
                        &format!("Write(Vault(\"{zone}\"))"),
                    );
                    // If a notifier is wired, don't dead-end: raise a permission request the human can
                    // expand (Deny/Once/Session/Everywhere via Telegram). Without a notifier there's no
                    // one to ask, so keep the hard refusal.
                    if self.notifier.is_some() {
                        let path = self.write_permission_request(call, zone).await?;
                        return Ok(permission_request_message(&path, zone));
                    }
                    return Err(format!(
                        "not authorized: '{}' writes to zone '{zone}', and this session's grant \
                         does not include Write({zone}). Calling an MCP is not permission to write \
                         with it.",
                        call.name
                    ));
                }
            }

            if let Some(message) = self
                .proposal_if_risky(
                    call,
                    &mcp_name,
                    consequence,
                    write_zone.as_deref(),
                    &write_target,
                )
                .await?
            {
                return Ok(message);
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
    /// An already-pending proposal for this exact call, if one exists.
    ///
    /// Matches on the proposed action — tool name and arguments — rather than on any id, because the
    /// retry that creates the duplicate is a fresh attempt at the same action. Only `Pending`
    /// counts: an approved-and-archived proposal is finished, and a rejected one must not silently
    /// suppress a later request.
    ///
    /// A read failure is not an error here. If the directory cannot be scanned, falling through and
    /// creating a new proposal is the safe direction — a duplicate notification is a nuisance, a
    /// silently dropped approval request is a hole.
    async fn pending_proposal_for(
        &self,
        proposals_subdir: &std::path::Path,
        call: &ToolInvocation,
    ) -> Option<PathBuf> {
        let mut entries = tokio::fs::read_dir(proposals_subdir).await.ok()?;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "md") {
                continue;
            }
            let Ok(content) = tokio::fs::read_to_string(&path).await else {
                continue;
            };
            let Ok(proposal) = liberado_common::Proposal::from_note(&content) else {
                continue;
            };
            if proposal.status != liberado_common::ProposalStatus::Pending {
                continue;
            }
            if let ProposedAction::ToolCalls(calls) = &proposal.proposed_action
                && calls.len() == 1
                && calls[0].tool == call.name
                && calls[0].args == call.arguments
            {
                return Some(path);
            }
        }
        None
    }

    async fn write_proposal(
        &self,
        call: &ToolInvocation,
        rationale: &str,
    ) -> Result<PathBuf, String> {
        // Compact id so the stem fits Telegram's 50-char callback_data budget — the full
        // correlation lives in the proposal's `correlation_id` field below, not the stem. The old
        // `{correlation_base}-{nanos}` was 60 bytes for a `chat-delegate-<ulid>` correlation, over
        // the cap, so a write-class/high-consequence downgrade sent a plain, un-tappable
        // notification (mirrors the `write_permission_request` fix).
        let proposal_id = format!(
            "prop-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        );

        // `proposals_dir` is the vault's proposals/ directory (see this module's doc comment) — the
        // daemon's react() routes changes here into the same approve/execute pipeline the
        // dispatcher's own pre-flight proposals use.
        let proposals_subdir = self.proposals_dir.join(liberado_common::PROPOSALS_DIR);

        // A gated call comes back as a tool *result*, so the model sees the action did not happen
        // and tries again — reasonably. Without this, every attempt mints another proposal and the
        // human gets another notification for one intent: three taps for one file, live on
        // 2026-08-01 (`sub:30060787b5943f7b`, three proposals 43s apart, same path, same rationale).
        //
        // Identity is the *action* — same tool, same arguments, still pending — not the attempt.
        if let Some(existing) = self.pending_proposal_for(&proposals_subdir, call).await {
            tracing::info!(
                path = %existing.display(),
                tool = %call.name,
                "an equivalent proposal is already awaiting approval; not creating another"
            );
            return Ok(existing);
        }

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

        self.persist_proposal_note(&proposals_subdir, &proposal_path, &note)
            .await?;

        tracing::info!(
            path = %proposal_path.display(),
            tool = %call.name,
            "proposal written"
        );

        self.notify_proposal(call, &proposal_id, rationale, &proposal_path)
            .await;

        Ok(proposal_path)
    }

    /// Create the proposals directory (if needed) and write the note, honoring the injected
    /// failpoints. The proposal is the safety artifact — a failure here means the action was NOT
    /// executed and NO proposal exists, so it must reach the human loudly, not degrade silently.
    async fn persist_proposal_note(
        &self,
        proposals_subdir: &Path,
        proposal_path: &Path,
        note: &str,
    ) -> Result<(), String> {
        // Create the proposals directory if it doesn't exist.
        if self.fail_next_create_dir.swap(false, Ordering::Relaxed) {
            return Err("simulated create_dir_all failure — proposal was NOT saved".into());
        }
        if let Err(e) = tokio::fs::create_dir_all(proposals_subdir).await {
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

        if self.fail_next_write.swap(false, Ordering::Relaxed) {
            return Err("simulated write failure — proposal was NOT saved".into());
        }
        if let Err(e) = tokio::fs::write(proposal_path, note).await {
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
        Ok(())
    }

    /// Tell the human a proposal awaits review. Best-effort: the proposal is already safely on
    /// disk, so a failed notification only changes where the human hears about it, not whether
    /// the safety property (a human gets to review) holds.
    async fn notify_proposal(
        &self,
        call: &ToolInvocation,
        proposal_id: &str,
        rationale: &str,
        proposal_path: &Path,
    ) {
        let Some(notifier) = &self.notifier else {
            return;
        };
        let message = format!(
            "Liberado: a new proposal needs your review.\n{rationale}\nTool: {}\nSaved at: {}",
            call.name,
            proposal_path.display()
        );
        match notifier.notify_proposal(proposal_id, &message).await {
            // The human now has the proposal on their phone out-of-band — record it so a chat
            // surface can drop the redundant "this needs approval" reply (Gap 2). Only on a
            // confirmed send: a failed notify leaves the chat reply as the sole signal.
            Ok(()) => self.notified_deferral.store(true, Ordering::Relaxed),
            Err(e) => {
                tracing::warn!(error = %e, "failed to send proposal notification");
            }
        }
    }

    /// Like [`write_proposal`](Self::write_proposal), but for a **permission request**: the call is
    /// refused for a missing `Write(zone)` and we ask the human to expand the grant. Stamps the
    /// requested capability onto the proposal (signed, tamper-evident) and notifies with the four
    /// scope buttons. On approval the daemon applies the grant per the chosen scope and executes the
    /// carried call.
    async fn write_permission_request(
        &self,
        call: &ToolInvocation,
        zone: &str,
    ) -> Result<PathBuf, String> {
        let proposal_id = permission_request_id();
        let proposals_subdir = self.proposals_dir.join(liberado_common::PROPOSALS_DIR);
        let proposal_path = proposals_subdir.join(format!("{proposal_id}.md"));

        let proposal = self.sign_permission_proposal(&proposal_id, call, zone);

        if let Err(e) = tokio::fs::create_dir_all(&proposals_subdir).await {
            tracing::error!(path = %proposals_subdir.display(), error = %e, "failed to create proposals directory");
            return Err(format!(
                "could not create the proposals directory at {}: {e} — no permission request was saved",
                proposals_subdir.display()
            ));
        }
        if let Err(e) = tokio::fs::write(&proposal_path, proposal.to_note()).await {
            tracing::error!(path = %proposal_path.display(), error = %e, "failed to write permission request");
            return Err(format!(
                "could not save the permission request at {}: {e} — the action was NOT executed",
                proposal_path.display()
            ));
        }
        tracing::info!(path = %proposal_path.display(), tool = %call.name, %zone, "permission request written");

        self.notify_permission_request(&proposal_id, call, zone)
            .await;

        Ok(proposal_path)
    }

    /// Build the signed permission-request proposal: the blocked call as a `ProposedAction`, the
    /// requested capability stamped on before signing (so the signature covers it, tamper-evident),
    /// and the pool keyed so the daemon's grant lands on the right pool.
    fn sign_permission_proposal(
        &self,
        proposal_id: &str,
        call: &ToolInvocation,
        zone: &str,
    ) -> SignedProposal {
        let mut proposal = Proposal::pending(
            proposal_id,
            &self.correlation_base,
            "liberado-chat",
            ProposedAction::ToolCalls(vec![liberado_common::ToolCall {
                tool: call.name.clone(),
                args: call.arguments.clone(),
            }]),
            format!(
                "Permission request: '{}' needs Write access to zone '{zone}'.",
                call.name
            ),
        )
        .with_requested_grant(Capability::Write(Zone::vault(zone)));
        proposal.pool = Some(self.pool_name.clone());
        self.signer.sign(proposal)
    }

    /// Notify the human (when a notifier is attached) that a permission request awaits their
    /// decision, with the four scope buttons. A confirmed send records the out-of-band surfacing
    /// so the chat surface can drop the duplicate "grant permission" reply (Gap 2); a failed
    /// notify leaves the chat reply as the sole signal.
    async fn notify_permission_request(
        &self,
        proposal_id: &str,
        call: &ToolInvocation,
        zone: &str,
    ) {
        let Some(notifier) = &self.notifier else {
            return;
        };
        let message = format!(
            "Liberado needs permission.\n'{}' wants to write zone '{zone}', which its grant \
             doesn't include.\nApprove once, for this session, or everywhere?",
            call.name,
        );
        match notifier
            .notify_permission_request(proposal_id, &message)
            .await
        {
            Ok(()) => self.notified_deferral.store(true, Ordering::Relaxed),
            Err(e) => {
                tracing::warn!(error = %e, "failed to send permission-request notification");
            }
        }
    }
}

/// Compact permission-request id: it must fit Telegram's callback_data budget (the full
/// correlation lives in the proposal's `correlation_id` field, not the stem). The old
/// `perm-{correlation_base}-{nanos}` was 65 bytes for a `chat-delegate-<ulid>` correlation — over
/// the cap — so the buttons silently degraded to a plain, un-tappable notification.
fn permission_request_id() -> String {
    format!(
        "perm-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    )
}

/// The tool *result* returned for a raised permission request. Tells the model plainly that the
/// action is paused pending the human's grant decision — not a failure it should route around.
fn permission_request_message(path: &std::path::Path, zone: &str) -> String {
    format!(
        "PERMISSION REQUESTED — the action was NOT executed. It needs Write access to zone '{zone}', \
         which this session's grant doesn't include. A request was sent for approval \
         (once / this session / everywhere). Saved at: {}. Do not retry or invent a result; the \
         action runs automatically once the human approves.",
        path.display()
    )
}

/// The tool *result* returned for a downgraded high-consequence/sweeping call.
///
/// Addressed to the **model**, because that is who reads it — this is a tool result, not a
/// notification. The previous wording ("it is high-consequence and needs *your* approval") was
/// second person to a human, and produced exactly the two behaviours you would predict once a model
/// reads it instead. It retried, because the action "was NOT executed" reads as a failure; and it
/// tried to approve the proposal itself, because approval was described as its to give. Live on
/// 2026-08-01, turn 7: *"I see - the system is gating write operations behind proposals. Let me
/// approve the proposal by editing its status."* — followed by a `turbovault:edit_note` against the
/// proposal file.
///
/// So state plainly that this is not a failure, that a human decides out of band, and that neither
/// retrying nor editing the proposal will help — those being the two things it otherwise tries next.
fn proposal_message(path: &std::path::Path) -> String {
    format!(
        "PROPOSAL CREATED — this action is queued for a human to approve, and did not run now. This \
         is not an error and not a failure of your request. A human approves or rejects it out of \
         band, and it runs automatically if they approve. Do NOT retry this call, and do NOT edit \
         the proposal: you cannot approve it yourself, and retrying only creates duplicate requests \
         for the same action. Treat this step as handed off, say it is awaiting approval, and \
         continue with any remaining work that does not depend on it. Proposal saved at {}.",
        path.display()
    )
}

/// A compact rendering of the authority actually in force, for `authority_decision`'s `held`.
///
/// Deliberately terse and deliberately complete on the two axes that get denied in practice: which
/// MCPs may be called, and which zones may be written. "You hold X, you needed Y" is the whole
/// diagnosis; a full Debug dump of the set buries that in noise.
fn held_summary(caps: &CapabilitySet) -> String {
    let mcps = caps.granted_mcps();
    let writes: Vec<&str> = caps
        .capabilities
        .iter()
        .filter_map(|c| match c {
            Capability::Write(Zone::Vault(z) | Zone::Named(z)) => Some(z.as_str()),
            _ => None,
        })
        .collect();
    format!(
        "mcps=[{}] write_zones=[{}]",
        mcps.join(","),
        writes.join(",")
    )
}

#[cfg(test)]
#[path = "risk_gated_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "risk_gated_survivor_tests.rs"]
mod survivor_tests;
