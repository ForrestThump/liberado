//! The shipped `config.example/` is a claim about how this system should be configured, and nobody
//! was checking it.
//!
//! These assert against the files in `config.example/` rather than a hand-built `Policy`, because
//! the defect being guarded is *the shipped configuration being wrong* — a fixture that constructs
//! its own grants would pass no matter what those files say.

use std::path::PathBuf;

use liberado_common::Capability;
use liberado_config_loader::{Config, Policy};

fn shipped_policy() -> Policy {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "..",
        "config.example",
        "policy.toml",
    ]
    .iter()
    .collect();
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    toml::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// `AskHuman` is the capability to block on a person. A goal that holds it and has nobody attached
/// parks forever: unresumable and uncancellable across a daemon restart, holding a concurrency slot.
///
/// Four such orphans accumulated before anyone noticed, because a parked session looks exactly like
/// a busy one — and every autonomous goal the PR shepherd started died this way, within seconds, on
/// an intake question whose answer was already in its own prompt.
#[test]
fn the_unattended_coding_hat_cannot_interrupt_a_human() {
    let caps = shipped_policy().capabilities_for("coding-unattended");
    assert!(
        !caps.contains(&Capability::AskHuman),
        "coding-unattended must never hold AskHuman — an unattended goal that can ask will park \
         forever waiting for someone who is not there"
    );
    // Granting nothing at all would also pass the assertion above while being useless, so pin that
    // this hat is a *narrowed* coding hat rather than an empty one.
    assert!(
        !caps.capabilities.is_empty(),
        "coding-unattended must still carry the coding pack's read authority"
    );
}

/// The attended hat is the control: it *should* hold AskHuman. Without this, deleting the
/// capability everywhere would satisfy the test above and silently break interactive coding.
#[test]
fn the_attended_coding_hat_can_still_interrupt_a_human() {
    let caps = shipped_policy().capabilities_for("coding");
    assert!(
        caps.contains(&Capability::AskHuman),
        "the attended coding hat must keep AskHuman — intake clarifies before it builds (S7)"
    );
}

/// The attended local coding hat — the ACP bridge Paseo spawns on your own machine.
///
/// It *keeps* `AskHuman`: there is a human in the editor, and a question they can answer is the
/// point of an interactive session. That is the opposite of `coding-unattended` above, and the
/// pair is why this is a grant rather than a flag — same pack, two hats, different authority.
#[test]
fn the_local_coding_hat_keeps_ask_human() {
    let caps = shipped_policy().capabilities_for("coding-local");
    assert!(
        !caps.capabilities.is_empty(),
        "coding-local must be declared — the ACP bridge refuses to start coding mode without it"
    );
    assert!(
        caps.contains(&Capability::AskHuman),
        "coding-local is attended; withholding AskHuman here would be coding-unattended's rule"
    );
}

/// The shipped example config must actually deserialize.
///
/// It shipped broken: a `[[session_profiles]]` header was inserted into a *commented* block, so the
/// table existed with every field commented out. That is valid TOML — an empty table in an array of
/// tables — and fails serde with `missing field 'name'`, which is why a TOML-level check missed it.
/// `liberado config check` reads the live config dir, not this one, so nothing looked at the file
/// we hand to new users.
#[test]
fn the_shipped_example_topology_deserializes() {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "..",
        "config.example",
        "topology.toml",
    ]
    .iter()
    .collect();
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let topology: liberado_config_loader::Topology =
        toml::from_str(&text).unwrap_or_else(|e| panic!("example topology does not parse: {e}"));
    Config {
        topology,
        policy: shipped_policy(),
        tuning: Default::default(),
    }
    .validate()
    .unwrap_or_else(|e| panic!("config.example does not validate: {e}"));
}
