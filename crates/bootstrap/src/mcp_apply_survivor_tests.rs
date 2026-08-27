//! Survivor kills for `crates/bootstrap/src/mcp_apply.rs` (mutation campaign 2026-08).
//!
//! Covers the validation arms and accessors the inline suite missed: the
//! blank-HTTP-url and blank-docker-image rejection guards, `Display` for
//! [`McpApplyError`], and `LiveMcpController::catalog` handing back the seeded
//! catalog rather than a fresh empty one.

use super::*;
use liberado_common::capability::Consequence;
use liberado_config::McpTransport;

fn mcp(name: &str, enabled: bool, transport: McpTransport) -> McpConfig {
    McpConfig {
        name: name.into(),
        enabled,
        description: "test".into(),
        consequence: Consequence::Reversible,
        transport,
        default_zone: None,
        tools: Vec::new(),
        zone_from_arg: None,
        write_tools: Vec::new(),
        writes_vault: Some(false),
    }
}

#[test]
fn apply_error_display_carries_the_rejection_message() {
    let err = McpApplyError {
        message: "rejected: blank thing".to_string(),
    };
    assert_eq!(
        err.to_string(),
        "rejected: blank thing",
        "Display must surface the message; operators read it from logs"
    );
}

#[test]
fn controller_catalog_hands_back_the_seeded_catalog() {
    let catalog = Arc::new(CapabilityCatalog::new());
    let registry = McpRegistry::new();
    let desired = vec![mcp(
        "seeded-mcp",
        true,
        McpTransport::Stdio {
            command: "true".into(),
            args: vec![],
        },
    )];
    apply_mcp_peer_set(&catalog, &registry, &desired).expect("valid single stdio peer applies");

    let controller = LiveMcpController::new(catalog, registry);
    let handed = controller.catalog();
    assert!(
        handed.descriptors().iter().any(|d| d.name == "seeded-mcp"),
        "controller.catalog() must be the same catalog the apply seeded"
    );
}

#[test]
fn an_enabled_http_mcp_with_a_blank_url_is_rejected() {
    let catalog = Arc::new(CapabilityCatalog::new());
    let registry = McpRegistry::new();
    let desired = vec![mcp(
        "blank-url",
        true,
        McpTransport::Http {
            url: "   ".to_string(),
        },
    )];

    let err = apply_mcp_peer_set(&catalog, &registry, &desired)
        .expect_err("a whitespace-only HTTP url must not validate");
    assert!(
        err.message.contains("empty HTTP url"),
        "rejection must name the cause: {}",
        err.message
    );
    assert!(
        registry.names().is_empty() && catalog.descriptors().is_empty(),
        "a rejected apply must leave the previous (empty) live set untouched"
    );
}

#[test]
fn an_enabled_docker_mcp_with_a_blank_image_is_rejected() {
    let catalog = Arc::new(CapabilityCatalog::new());
    let registry = McpRegistry::new();
    let desired = vec![mcp(
        "blank-image",
        true,
        McpTransport::Docker {
            image: "   ".to_string(),
            command: None,
            args: vec![],
            volumes: vec![],
            env: vec![],
        },
    )];

    let err = apply_mcp_peer_set(&catalog, &registry, &desired)
        .expect_err("a whitespace-only docker image must not validate");
    assert!(
        err.message.contains("empty docker image"),
        "rejection must name the cause: {}",
        err.message
    );
    assert!(
        registry.names().is_empty() && catalog.descriptors().is_empty(),
        "a rejected apply must leave the previous (empty) live set untouched"
    );
}
