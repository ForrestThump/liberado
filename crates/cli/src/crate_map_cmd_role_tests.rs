//! Split from `crate_map_cmd.rs` for module-health boundaries.

use super::role_blurb;

/// Every role the crate map can emit has a real blurb, and an unknown role renders nothing —
/// the generated table would otherwise silently lose a section's explanation.
#[test]
fn every_role_has_a_blurb() {
    for role in [
        "foundation",
        "client",
        "kernel",
        "store",
        "pack",
        "service",
        "surface",
        "root",
        "tooling",
        "testing",
    ] {
        assert!(
            !role_blurb(role).is_empty(),
            "role {role:?} must have a blurb"
        );
    }
    assert_eq!(role_blurb("unknown-role"), "");
}
