//! Credential resolution and matching.
//!
//! Every token in this file is a literal generated for the test and matches nothing real.

use super::*;
use liberado_config::{ApiClientConfig, CredentialRef};

/// Serializes the env-var writes below. `cargo test` runs a crate's tests concurrently in one
/// binary, and an unguarded `set_var` is a flake that always passes when re-run alone.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const FAKE_HUMAN: &str = "test-human-0000000000000000";
const FAKE_CLIENT: &str = "test-client-1111111111111111";

#[test]
fn a_secret_token_never_prints_its_value() {
    let auth = ApiAuth::from_values(Some(FAKE_HUMAN), &[("cli", FAKE_CLIENT)]);
    let rendered = format!("{auth:?}");
    assert!(
        !rendered.contains(FAKE_HUMAN) && !rendered.contains(FAKE_CLIENT),
        "Debug leaked a token: {rendered}"
    );
    assert!(rendered.contains("redacted"), "{rendered}");
}

#[test]
fn the_human_token_authenticates_as_the_owner() {
    let auth = ApiAuth::from_values(Some(FAKE_HUMAN), &[]);
    assert_eq!(
        auth.authenticate_bearer(FAKE_HUMAN),
        Some(Principal::Human { via_cookie: false })
    );
    assert!(auth.is_human_token(FAKE_HUMAN));
}

#[test]
fn a_client_token_authenticates_but_is_not_the_owner() {
    let auth = ApiAuth::from_values(Some(FAKE_HUMAN), &[("dogfood-mcp", FAKE_CLIENT)]);
    let principal = auth
        .authenticate_bearer(FAKE_CLIENT)
        .expect("client token authenticates");
    assert_eq!(
        principal,
        Principal::Client {
            name: "dogfood-mcp".into()
        }
    );
    assert!(
        !principal.is_human(),
        "a client token must not be the owner"
    );
    assert!(!auth.is_human_token(FAKE_CLIENT));
}

#[test]
fn an_unknown_token_authenticates_as_nobody() {
    let auth = ApiAuth::from_values(Some(FAKE_HUMAN), &[("cli", FAKE_CLIENT)]);
    assert_eq!(auth.authenticate_bearer("test-wrong-222222222222"), None);
    assert_eq!(auth.authenticate_bearer(""), None);
    // A prefix of a real token must not match: the comparison is over the whole value.
    assert_eq!(auth.authenticate_bearer(&FAKE_HUMAN[..8]), None);
}

#[test]
fn no_configured_credential_reports_an_open_daemon() {
    assert!(ApiAuth::default().is_open());
    assert!(!ApiAuth::from_values(Some(FAKE_HUMAN), &[]).is_open());
    assert!(!ApiAuth::from_values(None, &[("cli", FAKE_CLIENT)]).is_open());
    assert!(!ApiAuth::from_values(None, &[("cli", FAKE_CLIENT)]).has_human_credential());
}

#[test]
fn a_credential_file_is_read_and_trimmed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("human.token");
    std::fs::write(&path, format!("{FAKE_HUMAN}\n")).unwrap();
    let api = liberado_config::ApiConfig {
        human: CredentialRef::file(&path),
        ..Default::default()
    };
    let auth = ApiAuth::resolve(&api).expect("file credential resolves");
    assert!(auth.is_human_token(FAKE_HUMAN));
}

#[test]
fn a_missing_environment_variable_is_a_startup_error_naming_the_variable() {
    let api = liberado_config::ApiConfig {
        human: CredentialRef::env("LIBERADO_TEST_TOKEN_THAT_IS_NEVER_SET"),
        ..Default::default()
    };
    let err = ApiAuth::resolve(&api).expect_err("an unset variable must refuse");
    let message = err.to_string();
    assert!(
        message.contains("LIBERADO_TEST_TOKEN_THAT_IS_NEVER_SET"),
        "{message}"
    );
    assert!(message.contains("not set"), "{message}");
}

#[test]
fn a_short_token_is_refused_rather_than_accepted_quietly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("truncated.token");
    std::fs::write(&path, "abc").unwrap();
    let api = liberado_config::ApiConfig {
        human: CredentialRef::file(&path),
        ..Default::default()
    };
    let err = ApiAuth::resolve(&api).expect_err("a 3-character token must refuse");
    assert!(err.to_string().contains("16 characters"), "{err}");
    assert!(
        !err.to_string().contains("abc"),
        "the error must not quote the token: {err}"
    );
}

#[test]
fn a_client_sharing_the_human_token_is_refused() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY(test): serialized by ENV_LOCK; removed before returning.
    unsafe {
        std::env::set_var("LIBERADO_TEST_HUMAN_A", FAKE_HUMAN);
        std::env::set_var("LIBERADO_TEST_CLIENT_A", FAKE_HUMAN);
    }
    let api = liberado_config::ApiConfig {
        human: CredentialRef::env("LIBERADO_TEST_HUMAN_A"),
        clients: vec![ApiClientConfig {
            name: "sneaky".into(),
            token: CredentialRef::env("LIBERADO_TEST_CLIENT_A"),
        }],
        ..Default::default()
    };
    let err = ApiAuth::resolve(&api).expect_err("a client must not reuse the owner token");
    unsafe {
        std::env::remove_var("LIBERADO_TEST_HUMAN_A");
        std::env::remove_var("LIBERADO_TEST_CLIENT_A");
    }
    assert!(err.to_string().contains("owner authority"), "{err}");
}

#[test]
fn two_clients_sharing_a_token_are_refused() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("LIBERADO_TEST_CLIENT_B", FAKE_CLIENT);
        std::env::set_var("LIBERADO_TEST_CLIENT_C", FAKE_CLIENT);
    }
    let api = liberado_config::ApiConfig {
        clients: vec![
            ApiClientConfig {
                name: "one".into(),
                token: CredentialRef::env("LIBERADO_TEST_CLIENT_B"),
            },
            ApiClientConfig {
                name: "two".into(),
                token: CredentialRef::env("LIBERADO_TEST_CLIENT_C"),
            },
        ],
        ..Default::default()
    };
    let err = ApiAuth::resolve(&api).expect_err("shared client tokens must refuse");
    unsafe {
        std::env::remove_var("LIBERADO_TEST_CLIENT_B");
        std::env::remove_var("LIBERADO_TEST_CLIENT_C");
    }
    assert!(err.to_string().contains("names its holder"), "{err}");
}

#[test]
fn an_empty_api_table_resolves_to_an_open_daemon() {
    let auth = ApiAuth::resolve(&liberado_config::ApiConfig::default()).expect("no credentials");
    assert!(auth.is_open());
}
