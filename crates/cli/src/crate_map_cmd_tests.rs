//! Split from `crate_map_cmd.rs` for module-health boundaries.

use super::*;
use tempfile::tempdir;

#[test]
fn generates_role_groups_and_dependencies() {
    let dir = tempdir().unwrap();
    let crate_dir = dir.path().join("crates/demo");
    fs::create_dir_all(&crate_dir).unwrap();
    fs::write(
        crate_dir.join("Cargo.toml"),
        r#"
[package]
name = "liberado-demo"
description = "A demo | crate"
[package.metadata.liberado]
role = "tooling"
[dependencies]
liberado-common.workspace = true
sysmap-core = { workspace = true }
serde = { workspace = true }
"#,
    )
    .unwrap();
    for (directory, name) in [
        ("common", "liberado-common"),
        ("sysmap-core", "sysmap-core"),
    ] {
        let dependency_dir = dir.path().join("crates").join(directory);
        fs::create_dir_all(&dependency_dir).unwrap();
        fs::write(
            dependency_dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\ndescription = \"dependency\"\n\
                     [package.metadata.liberado]\nrole = \"tooling\"\n"
            ),
        )
        .unwrap();
    }
    let untagged_dir = dir.path().join("crates/untagged");
    fs::create_dir_all(&untagged_dir).unwrap();
    fs::write(
        untagged_dir.join("Cargo.toml"),
        "[package]\nname = \"liberado-untagged\"\n",
    )
    .unwrap();
    let undescribed_dir = dir.path().join("crates/undescribed");
    fs::create_dir_all(&undescribed_dir).unwrap();
    fs::write(
        undescribed_dir.join("Cargo.toml"),
        "[package]\nname = \"liberado-undescribed\"\n\
             [package.metadata.liberado]\nrole = \"tooling\"\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("docs/spec/reference")).unwrap();
    let (text, count) = generate(dir.path()).unwrap();
    assert_eq!(count, 5);
    assert!(text.contains("## tooling"));
    let demo_row = text
        .lines()
        .find(|line| line.starts_with("| [`liberado-demo`]"))
        .expect("generated demo row");
    assert!(demo_row.contains("`liberado-common`"));
    assert!(demo_row.contains("`sysmap-core`"));
    assert!(!demo_row.contains("`serde`"));
    assert!(text.contains("A demo \\| crate"));
    assert!(text.contains("5 workspace crates."));
    assert!(text.contains("untagged (fix these"));
    assert!(text.contains("*(no description in Cargo.toml)*"));
    assert!(
        !text.contains(" as of "),
        "generated output must not change when the UTC date changes"
    );
}
