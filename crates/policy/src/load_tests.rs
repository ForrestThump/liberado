use super::*;
use crate::descriptor::{PolicyError, VaultScope};
use crate::effect::Effect;

#[test]
fn loader_reads_a_declared_tool() {
    let text = r#"
[[tools]]
server = "turbovault"
name = "write_note"
effects = ["write"]
vault = "from_arg"
from_arg = "path"
"#;
    let policy = load_toml(text).unwrap();
    let tool = policy.get("turbovault", "write_note").unwrap();
    assert!(tool.effects().contains(&Effect::Write));
    assert_eq!(
        tool.vault(),
        &VaultScope::Zones {
            zones: Vec::new(),
            from_arg: Some("path".into()),
        }
    );
}

#[test]
fn loader_rejects_empty_effects() {
    let text = r#"
[[tools]]
server = "turbovault"
name = "write_note"
effects = []
vault = "none"
"#;
    let error = load_toml(text).unwrap_err();
    assert_eq!(
        error,
        PolicyError::UndeclaredEffects {
            server: "turbovault".into(),
            name: "write_note".into(),
        }
    );
}

#[test]
fn loader_rejects_duplicate_tools() {
    let text = r#"
[[tools]]
server = "turbovault"
name = "write_note"
effects = ["write"]
vault = "none"

[[tools]]
server = "turbovault"
name = "write_note"
effects = ["read"]
vault = "none"
"#;
    let error = load_toml(text).unwrap_err();
    assert_eq!(
        error,
        PolicyError::Duplicate {
            server: "turbovault".into(),
            name: "write_note".into(),
        }
    );
}

#[test]
fn loader_rejects_an_unknown_effect() {
    let text = r#"
[[tools]]
server = "turbovault"
name = "write_note"
effects = ["explode"]
vault = "none"
"#;
    let error = load_toml(text).unwrap_err();
    assert!(matches!(error, PolicyError::Toml(_)));
}

#[test]
fn loader_rejects_a_missing_effects_field() {
    let text = r#"
[[tools]]
server = "turbovault"
name = "write_note"
vault = "none"
"#;
    let error = load_toml(text).unwrap_err();
    assert!(matches!(error, PolicyError::Toml(_)));
}

#[test]
fn loader_rejects_an_unknown_field() {
    let text = r#"
[[tools]]
server = "turbovault"
name = "write_note"
effects = ["read"]
vault = "none"
surprise = true
"#;
    let error = load_toml(text).unwrap_err();
    assert!(matches!(error, PolicyError::Toml(_)));
}

#[test]
fn loader_rejects_a_parent_segment_in_an_allowlisted_url() {
    let text = r#"
[[tools]]
server = "stripe"
name = "charge"
effects = ["external"]
vault = "none"
urls = ["https://api.example/../admin"]
"#;
    let error = load_toml(text).unwrap_err();
    assert!(matches!(error, PolicyError::Url(_)));
}

#[test]
fn loader_rejects_zones_that_differ_only_by_case() {
    let text = r#"
[[tools]]
server = "turbovault"
name = "write_note"
effects = ["write"]
vault = "zones"
zones = ["tasks", "Tasks"]
"#;
    let error = load_toml(text).unwrap_err();
    assert!(matches!(error, PolicyError::ZoneCaseCollision { .. }));
}

#[test]
fn empty_tool_list_denies_every_call() {
    let policy = load_toml("tools = []\n").unwrap();
    assert!(policy.is_empty());
}
