use std::collections::BTreeSet;

use super::*;
use crate::descriptor::{Declaration, ToolPolicy, VaultScope};
use crate::effect::Effect;
use crate::path::PathError;

fn effects(effect: Effect) -> BTreeSet<Effect> {
    BTreeSet::from([effect])
}

fn policy(decls: Vec<Declaration>) -> ToolPolicy {
    ToolPolicy::from_declarations(decls).unwrap()
}

fn decl(name: &str, effect_set: BTreeSet<Effect>, vault: VaultScope) -> Declaration {
    Declaration {
        server: "vault".into(),
        name: name.into(),
        effects: effect_set,
        vault,
        urls: Vec::new(),
        external_services: BTreeSet::new(),
    }
}

fn call(tool: &str, effect: Effect) -> ToolCall {
    ToolCall {
        server: "vault".into(),
        tool: tool.into(),
        effect,
        vault_paths: Vec::new(),
        urls: Vec::new(),
        external_services: Vec::new(),
    }
}

#[test]
fn declared_tool_with_matching_effect_is_allowed() {
    let policy = policy(vec![decl(
        "write_note",
        effects(Effect::Write),
        VaultScope::zones(vec!["tasks".into()]),
    )]);
    let mut call = call("write_note", Effect::Write);
    call.vault_paths = vec!["tasks/a.md".into()];
    let allowed = policy.authorize(&call).unwrap();
    assert_eq!(allowed.effect, Effect::Write);
    assert_eq!(allowed.vault_paths[0].as_relative(), "tasks/a.md");
}

#[test]
fn undeclared_tool_is_denied() {
    let policy = policy(vec![decl(
        "write_note",
        effects(Effect::Write),
        VaultScope::None,
    )]);
    let error = policy.authorize(&call("ghost", Effect::Read)).unwrap_err();
    assert_eq!(
        error,
        Deny::Undeclared {
            server: "vault".into(),
            tool: "ghost".into(),
        }
    );
}

#[test]
fn empty_effects_are_denied() {
    let policy = policy(vec![decl("write_note", BTreeSet::new(), VaultScope::None)]);
    let error = policy
        .authorize(&call("write_note", Effect::Write))
        .unwrap_err();
    assert_eq!(
        error,
        Deny::UndeclaredEffects {
            server: "vault".into(),
            tool: "write_note".into(),
        }
    );
}

#[test]
fn undeclared_effect_is_denied() {
    let policy = policy(vec![decl(
        "read_note",
        effects(Effect::Read),
        VaultScope::None,
    )]);
    let error = policy
        .authorize(&call("read_note", Effect::Write))
        .unwrap_err();
    assert_eq!(
        error,
        Deny::EffectNotDeclared {
            server: "vault".into(),
            tool: "read_note".into(),
            effect: Effect::Write,
        }
    );
}

#[test]
fn vault_path_outside_declared_zone_is_denied() {
    let policy = policy(vec![decl(
        "write_note",
        effects(Effect::Write),
        VaultScope::zones(vec!["tasks".into()]),
    )]);
    let mut call = call("write_note", Effect::Write);
    call.vault_paths = vec!["Tasks/a.md".into()];
    let error = policy.authorize(&call).unwrap_err();
    assert!(matches!(
        error,
        Deny::Path(PathError::ZoneCaseMismatch { .. })
    ));
}

#[test]
fn path_addressed_call_without_a_path_is_denied() {
    let policy = policy(vec![decl(
        "write_note",
        effects(Effect::Write),
        VaultScope::from_arg("path"),
    )]);
    let error = policy
        .authorize(&call("write_note", Effect::Write))
        .unwrap_err();
    assert_eq!(
        error,
        Deny::MissingVaultPath {
            server: "vault".into(),
            tool: "write_note".into(),
        }
    );
}

#[test]
fn vault_wide_tool_rejects_a_path() {
    let policy = policy(vec![decl(
        "reindex",
        effects(Effect::Write),
        VaultScope::VaultWide,
    )]);
    let mut call = call("reindex", Effect::Write);
    call.vault_paths = vec!["tasks/a.md".into()];
    let error = policy.authorize(&call).unwrap_err();
    assert!(matches!(error, Deny::UnexpectedVaultPath { .. }));
}

#[test]
fn none_scope_rejects_a_path() {
    let policy = policy(vec![decl(
        "charge",
        effects(Effect::Write),
        VaultScope::None,
    )]);
    let mut call = call("charge", Effect::Write);
    call.vault_paths = vec!["tasks/a.md".into()];
    assert!(matches!(
        policy.authorize(&call),
        Err(Deny::UnexpectedVaultPath { .. })
    ));
}

#[test]
fn url_outside_allowlist_is_denied() {
    let mut declaration = decl("charge", effects(Effect::External), VaultScope::None);
    declaration.urls = vec!["https://api.example/v1/".into()];
    let policy = policy(vec![declaration]);
    let mut call = call("charge", Effect::External);
    call.urls = vec!["https://api.example/v2/charges".into()];
    let error = policy.authorize(&call).unwrap_err();
    assert!(matches!(error, Deny::UrlNotAllowed { .. }));
}

#[test]
fn declared_url_is_allowed() {
    let mut declaration = decl("charge", effects(Effect::External), VaultScope::None);
    declaration.urls = vec!["https://API.Example/v1/".into()];
    let policy = policy(vec![declaration]);
    let mut call = call("charge", Effect::External);
    call.urls = vec!["https://api.example/v1/charges".into()];
    let allowed = policy.authorize(&call).unwrap();
    assert_eq!(allowed.urls[0].host(), "api.example");
    assert_eq!(allowed.urls[0].path(), "/v1/charges");
}

#[test]
fn undeclared_external_service_is_denied() {
    let mut declaration = decl("charge", effects(Effect::External), VaultScope::None);
    declaration.external_services = BTreeSet::from(["stripe".into()]);
    let policy = policy(vec![declaration]);
    let mut call = call("charge", Effect::External);
    call.external_services = vec!["Stripe".into()];
    let error = policy.authorize(&call).unwrap_err();
    assert_eq!(
        error,
        Deny::ServiceNotDeclared {
            service: "Stripe".into()
        }
    );
}

#[test]
fn declared_external_service_is_allowed() {
    let mut declaration = decl("charge", effects(Effect::External), VaultScope::None);
    declaration.external_services = BTreeSet::from(["stripe".into()]);
    let policy = policy(vec![declaration]);
    let mut call = call("charge", Effect::External);
    call.external_services = vec!["stripe".into()];
    let allowed = policy.authorize(&call).unwrap();
    assert_eq!(allowed.external_services, vec!["stripe".to_owned()]);
}
