//! `liberado-server explain-write`: the static would-this-write-be-allowed explainer.
//!
//! Split from `lib.rs` for module-health boundaries. The pure half (`write_explanation`)
//! computes every guard's verdict from config alone and is unit-tested in
//! `explain_write_tests.rs`; `explain_write` only loads config and prints.

use std::path::Path;

/// # Why this exists
///
/// A tool call passes several independent guards, and until now **every one of them could say no
/// and none of them could say "it was me"**. Worse, a refusal and a deliberately-protected zone
/// produce the identical observable: a proposal. So a missing grant, a misdeclared MCP, and a
/// working policy are indistinguishable from outside — which is how a capability bug that denied
/// every subagent write survived months of use while the daemon logged that the grant was present.
///
/// `authority_decision` fixed that at runtime; this answers the same question *before* you deploy,
/// which is the difference between "run it and read the logs" and "ask".
///
/// Prints every guard's verdict rather than stopping at the first failure — the first `no` is
/// rarely the only one, and fixing them one deploy at a time is the slow path.
/// The structured answer [`explain_write`] prints: one line per guard that ran, how to fix every
/// blocker, and the verdict.
///
/// Plain data rather than side effects on stdout so tests can assert on each guard's verdict
/// without capturing stdout; the printing half of `explain_write` is too thin to hold decisions.
#[derive(Debug)]
pub(crate) struct WriteExplanation {
    /// The "would X be allowed …" question line (with its trailing blank line, as printed).
    header: String,
    /// One formatted line per guard that ran, in run order.
    guard_lines: Vec<String>,
    /// The final `verdict: …` line.
    verdict: String,
    /// How to fix each blocker, in blocker order; empty when allowed.
    fixes: Vec<String>,
}

/// Compute [`explain_write`]'s answer from config alone — no I/O, no printing.
///
/// Every branch of the explainer lives here so the guard pipeline it mirrors can be exercised
/// directly: an undeclared MCP short-circuits; then the grant, write-target, zone-write-class, and
/// consequence guards each run and *accumulate* blockers rather than stopping at the first.
pub(crate) fn write_explanation(
    config: &liberado_bootstrap::Config,
    component: &str,
    qualified_tool: &str,
    path: &str,
) -> WriteExplanation {
    use liberado_common::{Capability, WriteTarget, bare_tool_name, mcp_of};

    let header =
        format!("would `{component}` be allowed to call `{qualified_tool}` on `{path}`?\n");
    let mcp_name = mcp_of(qualified_tool);
    let bare = bare_tool_name(qualified_tool);
    let caps = config.policy.capabilities_for(component);
    // The same descriptor snapshot the live catalog is seeded from at boot, so this answers with
    // exactly the declarations the daemon would enforce — not a re-derivation that could disagree.
    let catalog = liberado_config::catalog_from_config(config);

    let mut guards = WriteExplanation {
        header,
        guard_lines: Vec::new(),
        verdict: String::new(),
        fixes: Vec::new(),
    };
    let mut blockers: Vec<String> = Vec::new();
    let say = |ok: bool| if ok { "PASS" } else { "BLOCK" };

    // 1. Is the MCP even declared?
    let Some(descriptor) = catalog.iter().find(|d| d.name == mcp_name).cloned() else {
        guards.guard_lines.push(format!(
            "  [BLOCK] mcp_declared      '{mcp_name}' is not an enabled [[mcps]] entry"
        ));
        guards.verdict = "verdict: BLOCKED — the MCP does not exist in this config.".into();
        return guards;
    };
    // `grants_tool`, because this explainer was asked about a *specific* tool and is echoed to a human
    // as a verdict. `grants_mcp` answers "is this MCP reachable at all", which for a partial grant is
    // true even when the named tool is not granted — an explainer that reports PASS on a call the
    // runtime would refuse is worse than no explainer.
    let granted = caps.grants_tool(qualified_tool);
    let needed = if caps.grants_mcp(mcp_name) {
        format!("ExecuteTool(\"{qualified_tool}\")")
    } else {
        format!("ExecuteMcp(\"{mcp_name}\") or ExecuteTool(\"{qualified_tool}\")")
    };
    guards.guard_lines.push(format!(
        "  [{}] mcp_grant         needed {needed}",
        say(granted)
    ));
    if !granted {
        blockers.push(format!(
            "add {{ ExecuteTool = \"{qualified_tool}\" }} (or {{ ExecuteMcp = \"{mcp_name}\" }} for \
             the whole server) to the '{component}' grant in policy.toml"
        ));
    }

    // 2. What does this call write, per the MCP's own declaration + these arguments?
    let args = serde_json::json!({ "path": path });
    let target = liberado_common::write_target(&descriptor, bare, &args);
    let zone = match &target {
        WriteTarget::NotAWrite => {
            guards.guard_lines.push(format!(
                "  [PASS] write_target      '{bare}' is a read on this MCP — no write guards apply"
            ));
            guards.verdict = if blockers.is_empty() {
                "verdict: ALLOWED".into()
            } else {
                "verdict: BLOCKED".into()
            };
            guards.fixes = blockers;
            return guards;
        }
        WriteTarget::Undeterminable(why) => {
            guards.guard_lines.push(format!(
                "  [BLOCK] write_target      cannot place this write: {why}"
            ));
            blockers.push(
                "give the path a leading zone segment, or declare zone_from_arg/write_tools"
                    .to_string(),
            );
            guards.verdict = "verdict: BLOCKED".into();
            guards.fixes = blockers;
            return guards;
        }
        WriteTarget::Zone(z) => z.clone(),
    };
    guards.guard_lines.push(format!(
        "  [PASS] write_target      resolves to zone '{zone}'"
    ));

    // 3. Does the component hold Write on that zone?
    let holds_write = caps.contains(&Capability::Write(liberado_common::Zone::vault(&zone)));
    guards.guard_lines.push(format!(
        "  [{}] write_capability  needed Write(Vault(\"{zone}\"))",
        say(holds_write)
    ));
    if !holds_write {
        blockers.push(format!(
            "add {{ Write = {{ Vault = \"{zone}\" }} }} to the '{component}' grant"
        ));
    }

    // 4. Is the zone itself directly agent-writable?
    let class = config.policy.write_class(&zone);
    let class_ok = class.allows_direct_agent_write();
    guards.guard_lines.push(format!(
        "  [{}] zone_write_class  zone '{zone}' is {class:?}{}",
        say(class_ok),
        if config.policy.zones.iter().any(|z| z.zone == zone) {
            ""
        } else {
            " (UNDECLARED — fail-safe default)"
        }
    ));
    if !class_ok {
        blockers.push(format!(
            "declare zone '{zone}' with write_class = \"agent_writable\" in policy.toml \
             (undeclared zones default to proposal_only)"
        ));
    }

    // 5. Consequence — proposal-gated rather than refused, but still not a direct write.
    let consequence = descriptor.consequence;
    let conseq_ok = consequence < liberado_common::CONSEQUENCE_GATE;
    guards.guard_lines.push(format!(
        "  [{}] consequence       '{mcp_name}' is {consequence:?} (gate is {:?})",
        say(conseq_ok),
        liberado_common::CONSEQUENCE_GATE
    ));
    if !conseq_ok {
        blockers.push(format!(
            "'{mcp_name}' is rated {consequence:?}, so every call is proposal-gated by design"
        ));
    }

    if blockers.is_empty() {
        guards.verdict = "verdict: ALLOWED — this write would execute directly.".into();
    } else {
        guards.verdict = format!("verdict: BLOCKED by {} guard(s):", blockers.len());
        guards.fixes = blockers;
    }
    guards
}

pub fn explain_write(
    dir: Option<&Path>,
    component: &str,
    qualified_tool: &str,
    path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let resolved = dir
        .map(Path::to_path_buf)
        .or_else(liberado_bootstrap::config_dir);
    let (config, _) = liberado_bootstrap::load_config(resolved.as_deref())?;

    let explanation = write_explanation(&config, component, qualified_tool, path);
    println!("{}", explanation.header);
    for line in &explanation.guard_lines {
        println!("{line}");
    }
    println!("\n{}", explanation.verdict);
    for fix in &explanation.fixes {
        println!("  fix: {fix}");
    }
    Ok(())
}

#[cfg(test)]
#[path = "explain_write_tests.rs"]
mod tests;
