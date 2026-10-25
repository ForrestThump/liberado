//! `[api]` parsing and validation.

use super::*;
use crate::model::Topology;

fn topology_with(api_table: &str) -> Topology {
    let text = format!("vault_path = \"/tmp/vault\"\n{api_table}");
    toml::from_str(&text).expect("topology parses")
}

#[test]
fn absent_api_table_is_loopback_with_no_credentials() {
    let topology: Topology = toml::from_str(r#"vault_path = "/tmp/vault""#).unwrap();
    assert_eq!(topology.api.bind, None);
    assert!(!topology.api.has_credentials());
    assert!(topology.api.allowed_origins.is_empty());
    assert!(topology.api.problems().is_empty());
    assert_eq!(DEFAULT_BIND, "127.0.0.1:4201");
}

#[test]
fn credentials_name_a_source_and_never_a_value() {
    let topology = topology_with(
        r#"
[api]
bind = "100.64.0.3:4201"
human = { env = "LIBERADO_HUMAN_TOKEN" }

[[api.clients]]
name = "operator-dogfood-mcp"
token = { file = "/run/secrets/liberado-dogfood-token" }
"#,
    );
    assert_eq!(topology.api.bind.as_deref(), Some("100.64.0.3:4201"));
    assert_eq!(
        topology.api.human.env.as_deref(),
        Some("LIBERADO_HUMAN_TOKEN")
    );
    assert!(topology.api.has_credentials());
    assert_eq!(topology.api.clients.len(), 1);
    assert_eq!(topology.api.clients[0].name, "operator-dogfood-mcp");
    assert!(topology.api.problems().is_empty());
}

/// A token literal in the config file is not a supported shape, and serde says so rather than
/// quietly ignoring the field — a config that looks accepted and grants nothing is worse than
/// one that refuses to load.
#[test]
fn an_inline_token_value_is_refused_by_the_schema() {
    let err = toml::from_str::<Topology>(
        "vault_path = \"/tmp/vault\"\n[api]\nhuman = { value = \"hunter2\" }\n",
    )
    .expect_err("inline token value must not parse");
    assert!(
        err.to_string().contains("value"),
        "error should name the rejected field: {err}"
    );
}

#[test]
fn a_credential_naming_both_sources_is_a_problem() {
    let cred = CredentialRef {
        env: Some("LIBERADO_HUMAN_TOKEN".into()),
        file: Some("/run/secrets/token".into()),
    };
    assert!(
        cred.problem()
            .expect("naming both sources is a problem")
            .contains("exactly one")
    );
}

#[test]
fn duplicate_client_names_are_a_problem() {
    let api = ApiConfig {
        clients: vec![
            ApiClientConfig {
                name: "cli".into(),
                token: CredentialRef::env("A"),
            },
            ApiClientConfig {
                name: "cli".into(),
                token: CredentialRef::env("B"),
            },
        ],
        ..ApiConfig::default()
    };
    let problems = api.problems();
    assert!(
        problems
            .iter()
            .any(|p| p.contains("two entries named 'cli'")),
        "{problems:?}"
    );
}

#[test]
fn a_wildcard_origin_is_a_problem() {
    let api = ApiConfig {
        allowed_origins: vec!["*".into()],
        ..ApiConfig::default()
    };
    assert!(
        api.problems().iter().any(|p| p.contains("exact origins")),
        "{:?}",
        api.problems()
    );
}

#[test]
fn an_unset_human_credential_is_not_reported_as_malformed() {
    let api = ApiConfig::default();
    assert!(api.human.is_unset());
    assert!(api.problems().is_empty());
}
