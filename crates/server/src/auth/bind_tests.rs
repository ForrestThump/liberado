//! Bind resolution, and the refusal that is the whole point of it.

use super::*;
use liberado_config::ApiConfig;

fn plan(bind_env: Option<&str>, port_env: Option<&str>, api: &ApiConfig) -> BindPlan {
    resolve_bind(bind_env, port_env, api).expect("bind resolves")
}

#[test]
fn nothing_set_binds_loopback_on_4201() {
    let p = plan(None, None, &ApiConfig::default());
    assert_eq!(p.addr.to_string(), "127.0.0.1:4201");
    assert_eq!(p.source, BindSource::Default);
    assert!(is_loopback(&p.addr));
}

#[test]
fn the_bind_env_opts_in_to_a_tailnet_address() {
    let p = plan(Some("100.64.0.3:4201"), None, &ApiConfig::default());
    assert_eq!(p.addr.to_string(), "100.64.0.3:4201");
    assert_eq!(p.source, BindSource::BindEnv);
    assert!(!is_loopback(&p.addr));
}

#[test]
fn the_bind_env_outranks_config_which_outranks_the_port_env() {
    let api = ApiConfig {
        bind: Some("127.0.0.1:4300".into()),
        ..Default::default()
    };
    assert_eq!(
        plan(Some("127.0.0.1:4400"), Some("4500"), &api)
            .addr
            .to_string(),
        "127.0.0.1:4400"
    );
    assert_eq!(
        plan(None, Some("4500"), &api).addr.to_string(),
        "127.0.0.1:4300"
    );
    assert_eq!(
        plan(None, Some("4500"), &ApiConfig::default())
            .addr
            .to_string(),
        "127.0.0.1:4500"
    );
}

/// The legacy port setting lives in every Compose file and deploy script here. Widening the
/// interface must stay a thing someone writes down, so a port can only ever move the port.
#[test]
fn the_legacy_port_env_cannot_widen_the_interface() {
    let p = plan(None, Some("4201"), &ApiConfig::default());
    assert!(is_loopback(&p.addr), "LIBERADO_PORT must stay on loopback");
}

#[test]
fn an_empty_override_falls_through_instead_of_failing() {
    let p = plan(Some("   "), None, &ApiConfig::default());
    assert_eq!(p.addr.to_string(), "127.0.0.1:4201");
}

#[test]
fn localhost_is_accepted_and_is_loopback() {
    let p = plan(Some("localhost:4201"), None, &ApiConfig::default());
    assert_eq!(p.addr.to_string(), "127.0.0.1:4201");
    assert!(is_loopback(&p.addr));
}

#[test]
fn a_hostname_that_is_not_a_literal_address_is_refused() {
    let err = resolve_bind(
        Some("liberado.tailnet.ts.net:4201"),
        None,
        &ApiConfig::default(),
    )
    .expect_err("a resolvable name must not be accepted");
    assert!(err.to_string().contains("literal"), "{err}");
    assert!(err.to_string().contains(BIND_ENV), "{err}");
}

/// The defect this whole change exists for: a reachable control plane with nothing in front
/// of it. The daemon refuses to start rather than serve it.
#[test]
fn a_non_loopback_bind_with_no_credential_refuses_to_start() {
    let p = plan(Some("0.0.0.0:4201"), None, &ApiConfig::default());
    assert!(!is_loopback(&p.addr));
    let err = ensure_bind_is_credentialed(&p, false).expect_err("must refuse");
    let message = err.to_string();
    assert!(message.contains("0.0.0.0:4201"), "{message}");
    assert!(message.contains("no API credential"), "{message}");
    assert!(
        message.contains(BIND_ENV),
        "names the setting to fix: {message}"
    );
}

#[test]
fn a_tailnet_bind_with_a_credential_is_allowed() {
    let p = plan(Some("100.64.0.3:4201"), None, &ApiConfig::default());
    assert!(ensure_bind_is_credentialed(&p, true).is_ok());
}

#[test]
fn loopback_needs_no_credential() {
    let p = plan(None, None, &ApiConfig::default());
    assert!(ensure_bind_is_credentialed(&p, false).is_ok());
}

#[test]
fn the_unspecified_ipv6_address_is_not_loopback() {
    let p = plan(Some("[::]:4201"), None, &ApiConfig::default());
    assert!(!is_loopback(&p.addr), "[::] is reachable from other hosts");
    assert!(ensure_bind_is_credentialed(&p, false).is_err());
    let six = plan(Some("[::1]:4201"), None, &ApiConfig::default());
    assert!(is_loopback(&six.addr));
}
