//! Split from `coding_run.rs` for module-health boundaries.

use super::*;

fn toml_path(p: &Path) -> String {
    let escaped = p.display().to_string().replace('\\', "\\\\");
    format!("\"{escaped}\"")
}

fn write_topology(root: &Path, body: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("topology.toml"),
        format!(
            "vault_path = \"/tmp/vault\"\n\n\
                 [[projects]]\n\
                 name = \"{name}\"\n\
                 root = {root}\n\n\
                 {body}\n",
            name = "fixture",
            root = toml_path(root),
        ),
    )
    .expect("write topology");
    dir
}

#[test]
fn interactive_spec_is_the_interactive_steps_not_ship() {
    let root = tempfile::tempdir().expect("tempdir");
    let cfg = write_topology(
        root.path(),
        "[projects.preflight.ship]\n\
             steps = [{ name = \"full\", run = \"echo ship-only\" }]\n\
             [projects.preflight.interactive]\n\
             steps = [{ name = \"light\", run = \"echo from-config\", timeout_secs = 30, required = false }]\n",
    );

    let spec = interactive_preflight_spec(Some(cfg.path()), root.path()).expect("spec");
    assert_eq!(spec.id, "interactive");
    assert_eq!(spec.steps.len(), 1);
    assert_eq!(spec.steps[0].name, "light");
    assert_eq!(spec.steps[0].run, "echo from-config");
    assert_eq!(spec.steps[0].timeout_secs, Some(30));
    assert!(!spec.steps[0].required);
    assert!(
        !spec.steps.iter().any(|s| s.run.contains("ship-only")),
        "done must not run the ship profile: {spec:?}"
    );
}

#[test]
fn ship_only_project_has_no_interactive_spec() {
    let root = tempfile::tempdir().expect("tempdir");
    let cfg = write_topology(
        root.path(),
        "[projects.preflight.ship]\n\
             steps = [{ name = \"full\", run = \"echo ship-only\" }]\n",
    );
    assert!(
        interactive_preflight_spec(Some(cfg.path()), root.path()).is_none(),
        "a ship bar must not silently become the interactive bar"
    );
}

#[test]
fn a_liberado_named_project_without_interactive_steps_has_no_spec() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = tempfile::tempdir().expect("config");
    std::fs::write(
        dir.path().join("topology.toml"),
        format!(
            "vault_path = \"/tmp/vault\"\n\n\
                 [[projects]]\n\
                 name = \"liberado\"\n\
                 root = {root}\n",
            root = toml_path(root.path()),
        ),
    )
    .expect("write topology");
    assert!(
        interactive_preflight_spec(Some(dir.path()), root.path()).is_none(),
        "interactive must not invent cargo (or any) steps from the project name"
    );
}

#[test]
fn undeclared_directory_has_no_interactive_spec() {
    let declared = tempfile::tempdir().expect("declared");
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let cfg = write_topology(
        declared.path(),
        "[projects.preflight.interactive]\n\
             steps = [{ name = \"light\", run = \"echo x\" }]\n",
    );
    assert!(interactive_preflight_spec(Some(cfg.path()), elsewhere.path()).is_none());
}

#[test]
fn no_config_dir_has_no_interactive_spec() {
    let root = tempfile::tempdir().expect("tempdir");
    assert!(interactive_preflight_spec(None, root.path()).is_none());
}
