//! Distro name and checkout pins for a Windows CRAP run.
//!
//! Each step stays small. The caller still bundles HEAD, maps the path through WSL, and
//! builds the cache layout from the same inputs as before.

use std::path::Path;

use super::CacheLayout;

pub(super) struct PinnedCheckout {
    pub(super) head: String,
    pub(super) base: String,
    pub(super) linux_bundle: String,
    pub(super) layout: CacheLayout,
}

pub(super) fn distro_name(value: Option<String>) -> String {
    value.unwrap_or_else(|| super::DEFAULT_DISTRO.to_string())
}

pub(super) fn pin_checkout(
    root: &Path,
    distro: &str,
    home: &str,
) -> Result<PinnedCheckout, Box<dyn std::error::Error>> {
    let revisions = revisions(root)?;
    let linux_bundle = linux_bundle(root, distro)?;
    let layout = layout_for(home, root)?;
    Ok(PinnedCheckout {
        head: revisions.head,
        base: revisions.base,
        linux_bundle,
        layout,
    })
}

struct Revisions {
    head: String,
    base: String,
}

fn revisions(root: &Path) -> Result<Revisions, Box<dyn std::error::Error>> {
    Ok(Revisions {
        head: super::super::git_text(root, &["rev-parse", "HEAD"])?,
        base: crate::ci_cmd::diff_gate::merge_base_sha(root, None)?,
    })
}

fn linux_bundle(root: &Path, distro: &str) -> Result<String, Box<dyn std::error::Error>> {
    let bundle = super::create_bundle(root)?;
    super::wsl_path(&bundle, distro)
}

fn layout_for(home: &str, root: &Path) -> Result<CacheLayout, Box<dyn std::error::Error>> {
    let key = super::cache_key(root)?;
    Ok(super::cache_layout(home, &key))
}

#[cfg(test)]
mod tests {
    use super::{distro_name, layout_for};

    #[test]
    fn a_missing_distro_uses_debian_and_a_set_value_wins() {
        assert_eq!(distro_name(None), "Debian");
        assert_eq!(distro_name(Some("Ubuntu".into())), "Ubuntu");
    }

    #[test]
    fn layout_for_keeps_the_driver_and_coverage_directories_apart() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout_for("/home/test", dir.path()).unwrap();
        assert!(
            layout
                .workspace
                .starts_with("/home/test/.cache/liberado/crap/")
        );
        assert!(layout.workspace.ends_with("/workspace"));
        assert_ne!(layout.driver_target, layout.coverage_target);
        assert!(layout.temp_dir.ends_with("/tmp"));
    }
}
