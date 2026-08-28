//! Keep non-Rust executable files small, public, and intentional.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Inventory {
    tool: Vec<Tool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tool {
    path: String,
    owner: String,
    purpose: String,
    justification: String,
}

#[test]
fn every_non_rust_tool_has_a_reviewed_boundary() {
    let root = workspace_root();
    let source = std::fs::read_to_string(root.join("non-rust-tools.toml")).unwrap();
    let inventory: Inventory = toml::from_str(&source).unwrap();
    let mut declared = BTreeMap::new();
    for tool in inventory.tool {
        assert!(!tool.owner.trim().is_empty(), "{} has no owner", tool.path);
        assert!(
            !tool.purpose.trim().is_empty(),
            "{} has no purpose",
            tool.path
        );
        assert!(
            tool.justification.trim().len() >= 40,
            "{} needs a substantive boundary justification",
            tool.path
        );
        assert!(
            declared.insert(tool.path.clone(), tool).is_none(),
            "duplicate inventory entry"
        );
    }

    let mut actual = Vec::new();
    collect_tools(&root, &root, &mut actual);
    actual.sort();
    let expected: Vec<_> = declared.keys().cloned().collect();
    assert_eq!(
        actual, expected,
        "update non-rust-tools.toml with this change"
    );

    for (path, _) in declared {
        let text = std::fs::read_to_string(root.join(&path))
            .unwrap()
            .to_ascii_lowercase();
        for private_marker in ["192.168.", "c:\\users\\shiloh", "shiloh@"] {
            assert!(
                !text.contains(private_marker),
                "{path} contains private deployment data: {private_marker}"
            );
        }
    }
}

#[test]
fn public_deployment_files_contain_no_private_host_data() {
    let root = workspace_root();
    for relative in [
        "config.example/ops.toml",
        "config.example/topology.toml",
        "deploy/homelab/README.md",
        "deploy/homelab/docker-compose.yml",
        "deploy/homelab/liberado-mcp-diagnosis.md",
        "docs/project/handoff.md",
    ] {
        let text = std::fs::read_to_string(root.join(relative))
            .unwrap()
            .to_ascii_lowercase();
        for private_marker in ["192.168.", "c:\\users\\shiloh", "/home/shiloh", "shiloh@"] {
            assert!(
                !text.contains(private_marker),
                "{relative} contains private deployment data: {private_marker}"
            );
        }
    }
}

fn collect_tools(root: &Path, directory: &Path, tools: &mut Vec<String>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if [".git", ".liberado", "target", "turbomcp", "turbovault"]
                .iter()
                .any(|ignored| name == *ignored)
            {
                continue;
            }
            collect_tools(root, &path, tools);
        } else if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("ps1" | "py" | "sh" | "js")
        ) {
            tools.push(
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf()
}
