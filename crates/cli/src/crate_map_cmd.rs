use std::fs;
use std::path::Path;

const ROLE_ORDER: &[&str] = &[
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
];

fn role_blurb(role: &str) -> &'static str {
    match role {
        "foundation" => {
            "The bottom layer: vocabulary and narrow-waist traits. Depends on nothing above itself."
        }
        "client" => {
            "Front-end building blocks, liftable into any UI without dragging the system along."
        }
        "kernel" => "The orchestration engine: decide/act loops, sessions, capability plumbing.",
        "store" => "Persistent and shared information: vault, conversations, memory, search.",
        "pack" => "Domain packs (coding first). Never sit beneath kernel/config/store layers.",
        "service" => "Out-of-process adapters: MCP servers, bots, the forge.",
        "surface" => "UIs. Clients of the wire contract only - enforced by layer_rules.rs.",
        "root" => "Composition roots: the only crates allowed to see everything.",
        "tooling" => {
            "Meta tooling (evals, heuristics tuner). Not build dependencies of the system."
        }
        "testing" => "Dev-dependency-only test support.",
        _ => "",
    }
}

#[derive(Debug, Default)]
struct CrateInfo {
    name: String,
    dir: String,
    description: String,
    role: String,
    deps: Vec<String>,
}

fn value(line: &str) -> Option<&str> {
    line.split_once('=')
        .map(|(_, value)| value.trim().trim_matches('"'))
}

fn read_crate(path: &Path) -> std::io::Result<Option<CrateInfo>> {
    let text = fs::read_to_string(path.join("Cargo.toml"))?;
    let mut info = CrateInfo {
        dir: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        ..Default::default()
    };
    let mut section = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed[1..trimmed.len() - 1].to_owned();
            continue;
        }
        if section == "package" {
            if trimmed.starts_with("name") {
                info.name = value(trimmed).unwrap_or_default().to_owned();
            } else if trimmed.starts_with("description") {
                info.description = value(trimmed).unwrap_or_default().to_owned();
            }
        } else if section == "package.metadata.liberado" && trimmed.starts_with("role") {
            info.role = value(trimmed).unwrap_or_default().to_owned();
        } else if section == "dependencies"
            && (trimmed.starts_with("liberado-") || trimmed.starts_with("chat-client-contract"))
            && trimmed.contains('=')
        {
            let dep = trimmed.split('=').next().unwrap_or_default().trim();
            if dep
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                info.deps.push(dep.to_owned());
            }
        }
    }
    Ok((!info.name.is_empty()).then_some(info))
}

fn escape_table(value: &str) -> String {
    value.replace('|', "\\|")
}

fn generate(root: &Path) -> std::io::Result<(String, usize)> {
    let mut crates = Vec::new();
    let crates_dir = root.join("crates");
    for entry in fs::read_dir(crates_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && let Some(info) = read_crate(&entry.path())?
        {
            crates.push(info);
        }
    }
    crates.sort_by(|a, b| a.name.cmp(&b.name));

    let mut out = String::new();
    out.push_str("# Crate map\n\n");
    out.push_str(
        "> **Generated file - do not edit.** Regenerate with `liberado docs crate-map`.\n",
    );
    out.push_str("> Source of truth: each crate's `Cargo.toml` (`description` + `[package.metadata.liberado] role`).\n");
    out.push_str("> Layer semantics and dependency rules: [contracts.md](../architecture/contracts.md) and\n");
    out.push_str("> `crates/test-support/tests/layer_rules.rs` (the same role tags, mechanically enforced).\n\n");
    let date = chrono::Utc::now().format("%Y-%m-%d");
    out.push_str(&format!(
        "{} workspace crates as of {}.\n",
        crates.len(),
        date
    ));

    for role in ROLE_ORDER {
        let mut group: Vec<&CrateInfo> = crates.iter().filter(|c| c.role == *role).collect();
        if group.is_empty() {
            continue;
        }
        group.sort_by(|a, b| a.name.cmp(&b.name));
        out.push_str(&format!("\n## {role}\n\n{}\n\n", role_blurb(role)));
        out.push_str("| Crate | Internal deps | Description |\n|---|---|---|\n");
        for c in group {
            let deps = if c.deps.is_empty() {
                "*none*".to_owned()
            } else {
                c.deps
                    .iter()
                    .map(|d| format!("`{d}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let description = if c.description.is_empty() {
                "*(no description in Cargo.toml)*".to_owned()
            } else {
                escape_table(&c.description)
            };
            out.push_str(&format!(
                "| [`{}`](../../../crates/{}/) | {} | {} |\n",
                c.name, c.dir, deps, description
            ));
        }
    }

    let untagged: Vec<&CrateInfo> = crates.iter().filter(|c| c.role.is_empty()).collect();
    if !untagged.is_empty() {
        out.push_str("\n## ⚠ untagged (fix these - layer_rules.rs will fail)\n");
        for c in untagged {
            out.push_str(&format!("- {}\n", c.name));
        }
    }
    Ok((out, crates.len()))
}

pub fn check_or_write(root: &Path, write: bool) -> Result<(), Box<dyn std::error::Error>> {
    let output = root.join("docs/spec/reference/crate-map.md");
    let (generated, count) = generate(root)?;
    if write {
        fs::write(&output, generated)?;
        println!("Wrote {} ({} crates)", output.display(), count);
    } else {
        let current = fs::read_to_string(&output)?;
        if current != generated {
            return Err(format!(
                "{} is stale; run `liberado docs crate-map --write`",
                output.display()
            )
            .into());
        }
        println!("Crate map: {} crates checked (up to date)", count);
    }
    Ok(())
}

pub fn repository_root() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let mut current = std::env::current_dir()?;
    loop {
        if current.join("Cargo.toml").is_file() && current.join("crates").is_dir() {
            return Ok(current);
        }
        if !current.pop() {
            return Err("could not find repository root (expected Cargo.toml and crates/)".into());
        }
    }
}

#[cfg(test)]
mod tests {
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
liberado-common = { workspace = true }
"#,
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("docs/spec/reference")).unwrap();
        let (text, count) = generate(dir.path()).unwrap();
        assert_eq!(count, 1);
        assert!(text.contains("## tooling"));
        assert!(text.contains("`liberado-common`"));
        assert!(text.contains("A demo \\| crate"));
    }
}
