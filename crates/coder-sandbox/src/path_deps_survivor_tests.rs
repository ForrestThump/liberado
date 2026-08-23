//! Split from `path_deps.rs` for module-health boundaries.

use super::*;

/// Segment selection must skip empty and `.` segments but keep normal first segments;
/// `..`-relative entries are ignored entirely (they live outside the shared root).
#[test]
fn path_dep_roots_take_the_first_real_segment() {
    let manifest = r#"
[workspace.dependencies]
core = { path = "turbovault/crates/turbovault-core" }
dotted = { path = "./plugins/local" }
updir = { path = "../outside/thing" }
plain = { path = "solo" }
"#;
    let mut roots = declared_path_dep_roots(manifest);
    roots.sort();
    assert_eq!(roots, vec!["plugins", "solo", "turbovault"]);
}
