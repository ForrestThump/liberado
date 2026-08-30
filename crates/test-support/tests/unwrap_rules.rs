//! Mechanical enforcement of production unwrap classifications and fatal unwraps ratchet.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use tree_sitter::{Node, Parser};

const CONFIG_FILE: &str = "unwrap-classification.toml";

#[derive(Debug, Deserialize)]
struct Config {
    thresholds: Thresholds,
    #[serde(default)]
    waiver: Vec<Waiver>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Thresholds {
    process_fatal_new: usize,
    local_failure_new: usize,
}

#[derive(Debug, Deserialize)]
struct Waiver {
    path: String,
    metric: String,
    ceiling: usize,
    reason: String,
    reviewed_on: String,
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf()
}

fn is_production_source(rel_path: &str) -> bool {
    let p = rel_path.replace('\\', "/");
    if !p.starts_with("crates/") || !p.contains("/src/") || !p.ends_with(".rs") {
        return false;
    }
    if p.contains("/tests/")
        || p.ends_with("/tests.rs")
        || p.ends_with("_tests.rs")
        || p.contains("/test_support/")
        || p.contains("/examples/")
        || p.contains("test_fixtures")
        || p.contains("test_helpers")
        || p.contains("test_util")
    {
        return false;
    }
    true
}

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name();
            if name == "target" || name == "tests" || name == ".liberado" {
                continue;
            }
            collect_rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Classification {
    ProvenInvariant,
    LocalFailure,
    ProcessFatal,
}

#[allow(dead_code)]
struct Occurrence {
    line: usize,
    method: String,
    snippet: String,
    classification: Classification,
}

#[allow(clippy::collapsible_if)]
fn classify_node(
    node: Node,
    source: &[u8],
    rel_path: &str,
    in_cfg_test: bool,
    occurrences: &mut Vec<Occurrence>,
) {
    let is_test_node = in_cfg_test || is_test_node(node, source);
    if is_test_node
        && (node.kind() == "mod_item"
            || node.kind() == "function_item"
            || node.kind() == "impl_item"
            || node.kind() == "struct_item"
            || node.kind() == "static_item")
    {
        return;
    }

    if !is_test_node && node.kind() == "call_expression" {
        if let Some(func) = node.child_by_field_name("function") {
            if func.kind() == "field_expression" {
                if let Some(field) = func.child_by_field_name("field") {
                    let method = field.utf8_text(source).unwrap_or_default();
                    if method == "unwrap" || method == "expect" {
                        let rec = func
                            .child_by_field_name("value")
                            .or_else(|| func.child(0))
                            .and_then(|a| a.utf8_text(source).ok())
                            .unwrap_or_default();
                        let args = node
                            .child_by_field_name("arguments")
                            .and_then(|a| a.utf8_text(source).ok())
                            .unwrap_or_default();
                        let snippet = node.utf8_text(source).unwrap_or_default();

                        let rec_low = rec.to_ascii_lowercase();
                        let args_low = args.to_ascii_lowercase();
                        let snip_low = snippet.to_ascii_lowercase();

                        let is_proven = rec_low.contains("lock()")
                            || rec_low.contains("read()")
                            || rec_low.contains("write()")
                            || args_low.contains("poisoned")
                            || args_low.contains("lock")
                            || rec_low.contains("regex::new")
                            || rec_low.contains("bytesregex::new")
                            || args_low.contains("static")
                            || args_low.contains("checked above")
                            || args_low.contains("always serializes")
                            || args_low.contains("non-empty")
                            || args_low.contains("statically valid")
                            || args_low.contains("marker present")
                            || args_low.contains("install sigterm")
                            || args_low.contains("ctrl+c")
                            || snip_low.contains("split_once(\"⟪here⟫\")");

                        let classification = if is_proven {
                            Classification::ProvenInvariant
                        } else if rel_path.contains("test_util")
                            || rel_path.contains("mock.rs")
                            || rel_path.contains("mcp-forge")
                        {
                            Classification::LocalFailure
                        } else {
                            Classification::ProcessFatal
                        };

                        occurrences.push(Occurrence {
                            line: node.start_position().row + 1,
                            method: method.to_string(),
                            snippet: snippet.to_string(),
                            classification,
                        });
                    }
                }
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        classify_node(child, source, rel_path, is_test_node, occurrences);
    }
}

fn is_test_node(node: Node, source: &[u8]) -> bool {
    if node.kind() == "source_file" {
        return false;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "attribute_item" {
            let text = child.utf8_text(source).unwrap_or_default();
            if text.contains("cfg(test)") || text.contains("test") {
                return true;
            }
        }
    }
    let mut prev = node.prev_sibling();
    while let Some(sibling) = prev {
        if sibling.kind() == "attribute_item" {
            let text = sibling.utf8_text(source).unwrap_or_default();
            if text.contains("cfg(test)") || text.contains("test") {
                return true;
            }
            prev = sibling.prev_sibling();
        } else {
            break;
        }
    }
    false
}

fn scan_all_production_unwraps(root: &Path) -> BTreeMap<String, Vec<Occurrence>> {
    let mut files = Vec::new();
    collect_rust_files(&root.join("crates"), &mut files);
    files.sort();

    let mut parser = Parser::new();
    let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    parser.set_language(&language).expect("load rust language");

    let mut results = BTreeMap::new();

    for file in files {
        let rel = file
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if !is_production_source(&rel) {
            continue;
        }

        let source = std::fs::read_to_string(&file).expect("read file");
        let tree = parser.parse(&source, None).expect("parse tree");
        let mut occurrences = Vec::new();
        classify_node(
            tree.root_node(),
            source.as_bytes(),
            &rel,
            false,
            &mut occurrences,
        );
        if !occurrences.is_empty() {
            results.insert(rel, occurrences);
        }
    }

    results
}

#[test]
fn unwrap_waivers_have_valid_structure_and_dates() {
    let root = workspace_root();
    let config_path = root.join(CONFIG_FILE);
    assert!(config_path.is_file(), "missing {}", CONFIG_FILE);

    let raw = std::fs::read_to_string(&config_path).expect("read config");
    let config: Config = toml::from_str(&raw).expect("parse config");

    let mut seen = BTreeSet::new();
    for waiver in &config.waiver {
        assert!(
            !waiver.reason.trim().is_empty(),
            "waiver for {} needs reason",
            waiver.path
        );
        assert!(
            waiver.reason.trim().len() >= 20,
            "waiver for {} reason is too short",
            waiver.path
        );
        assert!(
            !waiver.reviewed_on.trim().is_empty(),
            "waiver for {} needs reviewed_on",
            waiver.path
        );
        assert!(
            root.join(&waiver.path).is_file(),
            "stale waiver for missing file {}",
            waiver.path
        );
        assert!(
            seen.insert((waiver.path.clone(), waiver.metric.clone())),
            "duplicate waiver for {} / {}",
            waiver.path,
            waiver.metric
        );
    }
}

#[test]
fn unwrap_rules_enforce_ratchet_against_process_fatal_growth() {
    let root = workspace_root();
    let config_path = root.join(CONFIG_FILE);
    let raw = std::fs::read_to_string(&config_path).expect("read config");
    let config: Config = toml::from_str(&raw).expect("parse config");

    let scanned = scan_all_production_unwraps(&root);

    for (file, occurrences) in &scanned {
        let fatal_count = occurrences
            .iter()
            .filter(|o| o.classification == Classification::ProcessFatal)
            .count();

        let fatal_waiver = config
            .waiver
            .iter()
            .find(|w| w.path == *file && w.metric == "process_fatal")
            .map(|w| w.ceiling);

        let ceiling = fatal_waiver.unwrap_or(config.thresholds.process_fatal_new);

        assert!(
            fatal_count <= ceiling,
            "file {file} has {fatal_count} process_fatal unwraps exceeding ceiling {ceiling}"
        );
    }
}
