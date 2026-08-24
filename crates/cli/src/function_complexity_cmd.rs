//! Host-stable per-function complexity ratchet.

use liberado_common::process::std_command;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Stdio;

const CONFIG_FILE: &str = "function-complexity.toml";
const BASELINE_FILE: &str = "function-complexity-baseline.json";
const CURRENT_FILE: &str = ".liberado/function-complexity-current.json";
const TOOL_VERSION: &str = "0.4.3";

#[derive(Debug, Deserialize)]
struct Config {
    new_function_ceiling: f64,
    #[serde(default)]
    waiver: Vec<Waiver>,
}

#[derive(Debug, Deserialize)]
struct Waiver {
    file: String,
    function: String,
    ceiling: f64,
    reason: String,
    reviewed_on: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Report {
    #[serde(default)]
    version: String,
    entries: Vec<Entry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Entry {
    file: String,
    function: String,
    line: u64,
    cyclomatic: f64,
    coverage: Option<f64>,
    crap: f64,
    #[serde(default, rename = "crate")]
    crate_name: String,
}

type Key = (String, String, usize);

pub fn check(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config(root)?;
    generate(root)?;
    let baseline: Report = serde_json::from_slice(&std::fs::read(root.join(BASELINE_FILE))?)?;
    let current: Report = serde_json::from_slice(&std::fs::read(root.join(CURRENT_FILE))?)?;
    compare(&config, &baseline, &current)?;
    eprintln!(
        "[function complexity] ok: {} functions",
        current.entries.len()
    );
    Ok(())
}

pub fn ratchet(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if root.join(BASELINE_FILE).is_file() {
        check(root)?;
    } else {
        load_config(root)?;
        generate(root)?;
    }
    std::fs::copy(root.join(CURRENT_FILE), root.join(BASELINE_FILE))?;
    eprintln!("[function complexity] ratcheted {BASELINE_FILE}");
    Ok(())
}

fn load_config(root: &Path) -> Result<Config, Box<dyn std::error::Error>> {
    let config: Config = toml::from_str(&std::fs::read_to_string(root.join(CONFIG_FILE))?)?;
    if config.new_function_ceiling < 1.0 {
        return Err("new_function_ceiling must be at least 1".into());
    }
    let mut seen = BTreeSet::new();
    for waiver in &config.waiver {
        if waiver.reason.trim().is_empty() || waiver.reviewed_on.trim().is_empty() {
            return Err(format!(
                "complexity waiver for {} / {} needs reason and reviewed_on",
                waiver.file, waiver.function
            )
            .into());
        }
        if waiver.ceiling <= config.new_function_ceiling {
            return Err(format!(
                "complexity waiver for {} / {} is unnecessary",
                waiver.file, waiver.function
            )
            .into());
        }
        if !root.join(&waiver.file).is_file() {
            return Err(format!("stale complexity waiver: {}", waiver.file).into());
        }
        if !seen.insert((waiver.file.clone(), waiver.function.clone())) {
            return Err(format!(
                "duplicate complexity waiver for {} / {}",
                waiver.file, waiver.function
            )
            .into());
        }
    }
    Ok(config)
}

fn generate(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    require_tool(root)?;
    std::fs::create_dir_all(root.join(".liberado"))?;
    let status = std_command("cargo")
        .args([
            "crap",
            "--workspace",
            "--missing",
            "pessimistic",
            "--threshold",
            "1000000000",
            "--format",
            "json",
            "--sort",
            "file",
            "--output",
            CURRENT_FILE,
        ])
        .current_dir(root)
        .status()?;
    if status.success() {
        crate::ci_cmd::relativize_json_file(root, CURRENT_FILE)
    } else {
        Err("cargo crap could not generate the function-complexity report".into())
    }
}

fn require_tool(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let status = std_command("cargo")
        .args(["crap", "--version"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(value) if value.success() => Ok(()),
        _ => Err(format!(
            "cargo-crap is required. Run: cargo install cargo-crap --version {TOOL_VERSION} --locked"
        )
        .into()),
    }
}

fn compare(
    config: &Config,
    baseline: &Report,
    current: &Report,
) -> Result<(), Box<dyn std::error::Error>> {
    let old = indexed_complexities(&baseline.entries);
    let waivers: BTreeMap<(String, String), f64> = config
        .waiver
        .iter()
        .map(|waiver| {
            (
                (waiver.file.clone(), waiver.function.clone()),
                waiver.ceiling,
            )
        })
        .collect();
    let mut failures = Vec::new();
    let mut occurrences = BTreeMap::new();
    for entry in &current.entries {
        let pair = (entry.file.clone(), entry.function.clone());
        let occurrence = occurrences.entry(pair.clone()).or_insert(0usize);
        let key = (pair.0.clone(), pair.1.clone(), *occurrence);
        *occurrence += 1;
        let limit = waivers.get(&pair).copied().unwrap_or_else(|| {
            old.get(&key)
                .copied()
                .unwrap_or(config.new_function_ceiling)
        });
        if entry.cyclomatic > limit + 0.01 {
            let kind = if old.contains_key(&key) {
                "regressed"
            } else {
                "new"
            };
            failures.push(format!(
                "{kind} {} / {}: cyclomatic {:.0} > {:.0}",
                entry.file, entry.function, entry.cyclomatic, limit
            ));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "function-complexity regression:\n{}\nSplit the function or add a narrow reviewed waiver; do not raise the baseline.",
            failures.join("\n")
        )
        .into())
    }
}

fn indexed_complexities(entries: &[Entry]) -> BTreeMap<Key, f64> {
    let mut occurrences = BTreeMap::new();
    let mut indexed = BTreeMap::new();
    for entry in entries {
        let pair = (entry.file.clone(), entry.function.clone());
        let occurrence = occurrences.entry(pair.clone()).or_insert(0usize);
        indexed.insert((pair.0, pair.1, *occurrence), entry.cyclomatic);
        *occurrence += 1;
    }
    indexed
}

#[cfg(test)]
mod tests {
    use super::{CONFIG_FILE, Config, Entry, Report, Waiver, compare};

    fn entry(function: &str, cyclomatic: f64) -> Entry {
        Entry {
            file: "src/lib.rs".into(),
            function: function.into(),
            line: 1,
            cyclomatic,
            coverage: Some(0.0),
            crap: cyclomatic * cyclomatic + cyclomatic,
            crate_name: "demo".into(),
        }
    }

    fn config() -> Config {
        Config {
            new_function_ceiling: 20.0,
            waiver: vec![],
        }
    }

    #[test]
    fn existing_functions_may_not_gain_a_branch() {
        let baseline = Report {
            version: String::new(),
            entries: vec![entry("work", 3.0)],
        };
        let current = Report {
            version: String::new(),
            entries: vec![entry("work", 4.0)],
        };
        assert!(compare(&config(), &baseline, &current).is_err());
    }

    #[test]
    fn reviewed_waiver_has_an_explicit_ceiling() {
        let mut config = config();
        config.waiver.push(Waiver {
            file: "src/lib.rs".into(),
            function: "table".into(),
            ceiling: 25.0,
            reason: "declarative table".into(),
            reviewed_on: "2026-08-21".into(),
        });
        let baseline = Report {
            version: String::new(),
            entries: vec![],
        };
        let current = Report {
            version: String::new(),
            entries: vec![entry("table", 24.0)],
        };
        assert!(compare(&config, &baseline, &current).is_ok());
    }

    // ── load_config: the waiver validation ladder ───────────────────────

    fn config_dir_with(body: &str, extra_file: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), body).unwrap();
        if let Some(rel) = extra_file {
            std::fs::create_dir_all(dir.path().join(rel).parent().unwrap()).unwrap();
            std::fs::write(dir.path().join(rel), "// exists").unwrap();
        }
        dir
    }

    #[test]
    fn load_config_accepts_a_valid_waiver_against_an_existing_file() {
        let dir = config_dir_with(
            r#"
new_function_ceiling = 20.0

[[waiver]]
file = "src/lib.rs"
function = "big_one"
ceiling = 40.0
reason = "legacy parser state machine"
reviewed_on = "2026-08-01"
"#,
            Some("src/lib.rs"),
        );
        let config = super::load_config(dir.path()).unwrap();
        assert_eq!(config.waiver.len(), 1);
    }

    #[test]
    fn load_config_rejects_a_ceiling_below_one() {
        let dir = config_dir_with("new_function_ceiling = 0.5", None);
        let err = super::load_config(dir.path()).unwrap_err().to_string();
        assert!(err.contains("at least 1"), "{err}");
    }

    #[test]
    fn load_config_rejects_a_waiver_without_reason_or_review_date() {
        let dir = config_dir_with(
            r#"
new_function_ceiling = 20.0

[[waiver]]
file = "src/lib.rs"
function = "big_one"
ceiling = 40.0
reason = ""
reviewed_on = "2026-08-01"
"#,
            Some("src/lib.rs"),
        );
        let err = super::load_config(dir.path()).unwrap_err().to_string();
        assert!(err.contains("needs reason and reviewed_on"), "{err}");
    }

    #[test]
    fn load_config_rejects_an_unnecessary_waiver() {
        let dir = config_dir_with(
            r#"
new_function_ceiling = 20.0

[[waiver]]
file = "src/lib.rs"
function = "mild_one"
ceiling = 20.0
reason = "not above the default ceiling"
reviewed_on = "2026-08-01"
"#,
            Some("src/lib.rs"),
        );
        let err = super::load_config(dir.path()).unwrap_err().to_string();
        assert!(err.contains("unnecessary"), "{err}");
    }

    #[test]
    fn load_config_rejects_a_stale_waiver_for_a_deleted_file() {
        let dir = config_dir_with(
            r#"
new_function_ceiling = 20.0

[[waiver]]
file = "src/gone.rs"
function = "big_one"
ceiling = 40.0
reason = "the file is gone"
reviewed_on = "2026-08-01"
"#,
            None,
        );
        let err = super::load_config(dir.path()).unwrap_err().to_string();
        assert!(err.contains("stale complexity waiver"), "{err}");
    }

    #[test]
    fn load_config_rejects_duplicate_waivers() {
        let body = r#"
new_function_ceiling = 20.0

[[waiver]]
file = "src/lib.rs"
function = "big_one"
ceiling = 40.0
reason = "first"
reviewed_on = "2026-08-01"

[[waiver]]
file = "src/lib.rs"
function = "big_one"
ceiling = 41.0
reason = "second copy"
reviewed_on = "2026-08-01"
"#;
        let dir = config_dir_with(body, Some("src/lib.rs"));
        let err = super::load_config(dir.path()).unwrap_err().to_string();
        assert!(err.contains("duplicate"), "{err}");
    }
}
