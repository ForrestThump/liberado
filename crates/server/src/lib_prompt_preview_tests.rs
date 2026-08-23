//! Split from `lib.rs` for module-health boundaries.

use super::*;
use liberado_config::Config;

/// A config with one `basic-chat`-shaped profile: no dispatch, a nudge, one whole-server grant
/// and two named tools.
fn config_with_basic_chat() -> Config {
    let toml = r#"
vault_path = "/tmp/vault"

[main_agent]
delegation_mode = true

# Declared because the loader refuses a profile naming an MCP that does not exist — the fail-closed
# check that makes "config names a tool the toolset lacks" unrepresentable here.
[[mcps]]
name = "liberado-search-orchestrator-mcp"
description = "search"
consequence = "read_only"
transport = { kind = "http", url = "http://search:8080" }

[[mcps]]
name = "turbovault"
description = "vault"
consequence = "read_only"
transport = { kind = "http", url = "http://turbovault:3001" }

[[session_profiles]]
name          = "basic-chat"
delegation    = false
prompt_append = "Answer directly and briefly."
read  = []
write = []
mcps  = [
  "liberado-search-orchestrator-mcp",
  { name = "turbovault", tools = ["tasks_list"] },
]
"#;
    // Written to a real directory and loaded through the real loader, rather than assembled
    // in memory: the point of this command is to report what the *config* produces, so a
    // fixture that skipped parsing could pass while the file it stands for failed to load.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("topology.toml"), toml).expect("write topology");
    let (config, _) = liberado_config::load_config(Some(dir.path())).expect("fixture config");
    config
}

/// The bug this command exists for, catchable **from config, with no daemon**.
///
/// Live on 2026-07-28 a `basic-chat` session was handed the face-agent prompt — "you are a face
/// agent, not a tool user… call the `delegate` tool" — while holding no `delegate`. It announced
/// work and did none. Finding that cost a 17-minute build and a browserless run. This assertion
/// is the same finding in milliseconds.
#[test]
fn a_non_delegating_profile_is_not_described_as_a_face_agent() {
    let config = config_with_basic_chat();
    let preview = compose_chat_prompt(&config, Some("basic-chat")).unwrap();

    assert!(!preview.delegation);
    assert_eq!(
        preview.system_messages[0],
        liberado_main_agent::DEFAULT_SYSTEM_PROMPT
    );
    assert!(
        !preview.system_messages[0].contains("delegate"),
        "a chat that cannot delegate must not be told to call `delegate`"
    );
    let manifest = preview.system_messages.last().unwrap();
    assert!(
        !manifest.contains("delegate"),
        "and `delegate` must not appear in its tool list either: {manifest}"
    );
}

/// The composed order must mirror what `ChatSessions` injects per turn — base, nudge, manifest.
/// If these diverge the command reports a prompt nobody is ever given.
#[test]
fn the_composed_order_mirrors_the_turn() {
    let preview = compose_chat_prompt(&config_with_basic_chat(), Some("basic-chat")).unwrap();
    assert_eq!(preview.system_messages.len(), 3);
    assert_eq!(preview.system_messages[1], "Answer directly and briefly.");
    assert!(
        preview.system_messages[2].contains("available to you on this turn"),
        "the manifest must be last, as it is in the turn"
    );
}

/// Per-tool grants render exactly; whole-server grants cannot be resolved without a live daemon
/// and must say so rather than pretending to be a resolved name.
#[test]
fn whole_server_grants_are_marked_rather_than_faked() {
    let preview = compose_chat_prompt(&config_with_basic_chat(), Some("basic-chat")).unwrap();
    let manifest = preview.system_messages.last().unwrap();

    assert!(manifest.contains("turbovault:tasks_list"), "{manifest}");
    assert!(
        manifest.contains("liberado-search-orchestrator-mcp:*"),
        "an unexpandable grant must be visibly unexpanded: {manifest}"
    );
    assert_eq!(
        preview.unresolved_mcps,
        vec!["liberado-search-orchestrator-mcp".to_string()],
        "and the caller must be told which names are approximate"
    );
}

/// A chat naming no profile inherits the daemon's delegation mode and gets `delegate` — the
/// path every pre-existing conversation is on, so a regression here is the widest possible.
#[test]
fn no_profile_inherits_the_daemon_default() {
    let preview = compose_chat_prompt(&config_with_basic_chat(), None).unwrap();
    assert!(preview.delegation, "must inherit delegation_mode = true");
    assert_eq!(
        preview.system_messages[0],
        liberado_main_agent::HUMAN_INTERFACE_SYSTEM_PROMPT
    );
    assert!(
        preview
            .system_messages
            .last()
            .unwrap()
            .contains(liberado_main_agent::DELEGATE_TOOL_NAME),
        "a delegating chat's one tool is `delegate`"
    );
}

/// An unknown profile must be an error, not a silent fall-through to the default — the same
/// rule the switching endpoint enforces, for the same reason: a typo resolving to "no profile"
/// means quietly reporting the *wider* grant.
#[test]
fn an_unknown_profile_is_refused() {
    assert!(compose_chat_prompt(&config_with_basic_chat(), Some("nope")).is_err());
}
