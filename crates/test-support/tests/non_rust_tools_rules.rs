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
        "deploy/homelab/docker-compose.ghcr.yml",
        "deploy/homelab/docker-compose.ghcr-webui.yml",
        "deploy/homelab/setup.sh",
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
            let name_str = name.to_string_lossy();
            if [
                ".git",
                ".liberado",
                ".kilo",
                ".codex",
                ".claude",
                ".worktrees",
                ".pytest_cache",
                ".tmp",
                "target",
                "turbomcp",
                "turbovault",
                "subagent-manager-mcp-master",
                "node_modules",
                "cline",
                "deepagents",
                "grok-build",
                "harness-bench",
                "hermes-agent",
                "kimi-code",
                "oh-my-pi",
                "opencode",
                "paseo",
                "pi",
                "swarm-forge",
            ]
            .iter()
            .any(|ignored| {
                name_str == *ignored
                    || name_str.starts_with("liberado-") && path.parent() == Some(root)
            }) {
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

#[test]
fn homelab_setup_script_pulls_ghcr_and_does_not_write_host_config() {
    let root = workspace_root();
    let script = std::fs::read_to_string(root.join("deploy/homelab/setup.sh")).unwrap();
    let overlay =
        std::fs::read_to_string(root.join("deploy/homelab/docker-compose.ghcr.yml")).unwrap();
    let webui =
        std::fs::read_to_string(root.join("deploy/homelab/docker-compose.ghcr-webui.yml")).unwrap();

    assert!(script.contains("ghcr.io/forrestthump/liberado"));
    assert!(script.contains("docker pull"));
    assert!(script.contains("sha-${COMMIT_SHA}"));
    assert!(script.contains("--project-directory"));
    assert!(script.contains("--force-recreate"));
    assert!(script.contains("never writes"));
    assert!(!script.contains("topology.toml"));
    assert!(!script.contains("policy.toml"));
    assert!(!script.contains(">>"));
    assert!(overlay.contains("image: ${LIBERADO_IMAGE"));
    assert!(!overlay.contains("volumes:"));
    assert!(webui.contains("LIBERADO_WEBUI_DIST"));
    assert!(!webui.contains("volumes:"));
}

#[cfg(unix)]
#[test]
fn homelab_setup_dry_run_leaves_host_config_untouched() {
    let root = workspace_root();
    let temp = std::env::temp_dir().join(format!("liberado-setup-dry-run-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(temp.join("config")).unwrap();
    std::fs::write(temp.join("docker-compose.yml"), "services: {}\n").unwrap();
    std::fs::write(temp.join("config/topology.toml"), "provider = \"none\"\n").unwrap();
    std::fs::write(temp.join(".env"), "KEEP=secret\n").unwrap();
    let before_config = std::fs::read_to_string(temp.join("config/topology.toml")).unwrap();
    let before_env = std::fs::read_to_string(temp.join(".env")).unwrap();

    let status = std::process::Command::new("bash")
        .arg(root.join("deploy/homelab/setup.sh"))
        .arg("--dry-run")
        .env("LIBERADO_HOMELAB_DIR", &temp)
        .current_dir(&root)
        .status()
        .unwrap();
    assert!(status.success(), "setup.sh --dry-run failed");
    assert_eq!(
        std::fs::read_to_string(temp.join("config/topology.toml")).unwrap(),
        before_config
    );
    assert_eq!(
        std::fs::read_to_string(temp.join(".env")).unwrap(),
        before_env
    );
    let _ = std::fs::remove_dir_all(&temp);
}
