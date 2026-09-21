//! The shipped `config.example/` is a claim about how this system should be configured, and nobody
//! was checking it.
//!
//! These assert against the files in `config.example/` rather than a hand-built `Policy`, because
//! the defect being guarded is *the shipped configuration being wrong* — a fixture that constructs
//! its own grants would pass no matter what those files say.

use std::path::PathBuf;

use liberado_common::Capability;
use liberado_config_loader::{Config, Policy, Topology};

fn shipped_example_path(name: &str) -> PathBuf {
    [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "..",
        "config.example",
        name,
    ]
    .iter()
    .collect()
}

fn shipped_example_text(name: &str) -> String {
    let path = shipped_example_path(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn shipped_policy() -> Policy {
    let text = shipped_example_text("policy.toml");
    toml::from_str(&text).unwrap_or_else(|e| panic!("parse config.example/policy.toml: {e}"))
}

fn shipped_example_config() -> Config {
    let topology: Topology = toml::from_str(&shipped_example_text("topology.toml"))
        .unwrap_or_else(|e| panic!("parse config.example/topology.toml: {e}"));
    Config {
        topology,
        policy: shipped_policy(),
        tuning: Default::default(),
    }
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
    shipped_example_config()
        .validate()
        .unwrap_or_else(|e| panic!("config.example does not validate: {e}"));
}

/// CAS3: the named Chat-shelf hat exists, is enabled, has no pack domain, and borrows the
/// face grant. New Chat stays the default (no profile); this is the hat you pick with
/// `/profile`. It must not sit in `[chat] agent_profiles` or it lands on the Agents shelf.
#[test]
fn chat_default_is_an_enabled_chat_hat_on_the_main_agent_grant() {
    let cfg = shipped_example_config();
    let profile = cfg
        .topology
        .session_profiles
        .iter()
        .find(|p| p.name == "chat-default")
        .expect("config.example/topology.toml must name a chat-default profile");
    assert!(profile.enabled, "chat-default must be selectable");
    assert!(
        profile.domain.is_none(),
        "chat-default is a chat hat, not a pack profile"
    );
    assert_eq!(
        profile.component.as_deref(),
        Some("main-agent"),
        "chat-default borrows the face grant so the chat-search opt-in applies"
    );
    assert!(
        !cfg.tuning
            .chat
            .agent_profiles
            .iter()
            .any(|n| n == "chat-default"),
        "chat-default must stay off the Agents shelf (not in default agent_profiles)"
    );

    let resolved = cfg
        .resolve_session_profile(Some("chat-default"), "life")
        .expect("chat-default must resolve");
    assert_eq!(resolved.name.as_deref(), Some("chat-default"));
    assert!(resolved.domain.is_none());
    assert_eq!(
        resolved.capabilities,
        cfg.policy.capabilities_for("main-agent")
    );
}

/// CAS3: `chat-search` is operator opt-in on the face grant. Granting it by default
/// grows the face catalogue; omitting the comment leaves no opt-in path.
#[test]
fn main_agent_chat_search_is_commented_opt_in_not_granted() {
    let caps = shipped_policy().capabilities_for("main-agent");
    assert!(
        !caps.contains(&Capability::ExecuteMcp("chat-search".into())),
        "CAS3 ships chat-search as operator opt-in; granting it by default grows the face catalogue"
    );

    let text = shipped_example_text("policy.toml");
    let start = text
        .find("component = \"main-agent\"")
        .expect("main-agent grant missing from config.example/policy.toml");
    let rest = &text[start..];
    let end = rest.find("\n[[grants]]").unwrap_or(rest.len());
    let block = &rest[..end];
    assert!(
        block.contains("# { ExecuteMcp = \"chat-search\" }"),
        "CAS3 opt-in comment must live on the main-agent grant, not only dispatcher; block was:\n{block}"
    );
}
