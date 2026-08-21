//! Liberado's profile: the layer/kind vocabulary and runtime wiring, declared as data in
//! `sysmap.toml` rather than Rust. The generic `sysmap-core` parses and applies it; this module
//! only supplies the Liberado copy (embedded at compile time).

use sysmap_core::profile::Profile;

/// Parse the embedded `sysmap.toml`. A malformed profile is a programmer error — the file is
/// compiled in and validated by tests, so panic (not silently degrade).
pub fn liberado_profile() -> Profile {
    const RAW: &str = include_str!("../sysmap.toml");
    match Profile::from_toml_str(RAW) {
        Ok(profile) => profile,
        Err(e) => panic!("invalid embedded sysmap.toml: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_profile_parses() {
        let profile = liberado_profile();
        assert_eq!(profile.manifest_namespace, "liberado");
        assert_eq!(profile.layers.len(), 11);
        assert_eq!(profile.kinds.len(), 9);
        assert_eq!(profile.edges.len(), 1);
        assert_eq!(profile.edge_rules.len(), 9);
        assert_eq!(profile.routes.len(), 2);
        // Main-stack layers are the 8 flow layers, in order.
        let vocab = profile.vocabulary();
        let main: Vec<&str> = vocab.main_stack().map(|l| l.id.as_str()).collect();
        assert_eq!(
            main,
            [
                "foundation",
                "client",
                "kernel",
                "store",
                "pack",
                "service",
                "surface",
                "root"
            ]
        );
    }
}
