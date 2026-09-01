//! Tree-sitter-based Rust AST unwrap/expect classifier.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use tree_sitter::{Node, Parser};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    ProvenInvariant,
    LocalFailure,
    ProcessFatal,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UnwrapOccurrence {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub method: String,
    pub snippet: String,
    pub receiver: String,
    pub context: String,
    pub classification: Classification,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct FileUnwrapMetrics {
    pub proven_invariant: usize,
    pub local_failure: usize,
    pub process_fatal: usize,
    pub total: usize,
    pub occurrences: Vec<UnwrapOccurrence>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SummaryMetrics {
    pub total_unwraps: usize,
    pub proven_invariant: usize,
    pub local_failure: usize,
    pub process_fatal: usize,
    pub files_scanned: usize,
    pub files_with_unwraps: usize,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Report {
    pub summary: SummaryMetrics,
    pub files: BTreeMap<String, FileUnwrapMetrics>,
}

pub fn is_production_source(rel_path: &str) -> bool {
    let p = rel_path.replace('\\', "/");
    if !p.starts_with("crates/") || !p.contains("/src/") || !p.ends_with(".rs") {
        return false;
    }
    !is_test_support_path(&p)
}

fn is_test_support_path(path: &str) -> bool {
    path.contains("/tests/")
        || path.ends_with("/tests.rs")
        || path.ends_with("_tests.rs")
        || path.contains("/test_support/")
        || path.contains("/examples/")
        || path.contains("test_fixtures")
        || path.contains("test_helpers")
        || path.contains("test_util")
}

pub fn analyze_tree(root: &Path) -> Result<Report, Box<dyn std::error::Error>> {
    let mut files = Vec::new();
    collect_rust_files(&root.join("crates"), &mut files);
    files.sort();

    let mut report = Report::default();

    for file in files {
        let rel = file
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        if !is_production_source(&rel) {
            continue;
        }

        report.summary.files_scanned += 1;
        let source = std::fs::read_to_string(&file)?;
        let occurrences = classify_file(&rel, &source)?;

        if !occurrences.is_empty() {
            report.summary.files_with_unwraps += 1;
            let mut file_metrics = FileUnwrapMetrics {
                total: occurrences.len(),
                ..Default::default()
            };
            for occ in occurrences {
                match occ.classification {
                    Classification::ProvenInvariant => {
                        file_metrics.proven_invariant += 1;
                        report.summary.proven_invariant += 1;
                    }
                    Classification::LocalFailure => {
                        file_metrics.local_failure += 1;
                        report.summary.local_failure += 1;
                    }
                    Classification::ProcessFatal => {
                        file_metrics.process_fatal += 1;
                        report.summary.process_fatal += 1;
                    }
                }
                report.summary.total_unwraps += 1;
                file_metrics.occurrences.push(occ);
            }
            report.files.insert(rel, file_metrics);
        }
    }

    Ok(report)
}

fn collect_rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
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

pub fn classify_file(
    rel_path: &str,
    source: &str,
) -> Result<Vec<UnwrapOccurrence>, Box<dyn std::error::Error>> {
    let mut parser = Parser::new();
    let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    parser.set_language(&language)?;

    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return Ok(Vec::new()),
    };

    let mut occurrences = Vec::new();
    walk_node(
        tree.root_node(),
        source.as_bytes(),
        rel_path,
        false,
        &mut occurrences,
    );
    Ok(occurrences)
}

fn walk_node(
    node: Node,
    source: &[u8],
    rel_path: &str,
    in_cfg_test: bool,
    occurrences: &mut Vec<UnwrapOccurrence>,
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
        occurrences.extend(inspect_call(node, source, rel_path));
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_node(child, source, rel_path, is_test_node, occurrences);
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

fn inspect_call(node: Node, source: &[u8], rel_path: &str) -> Option<UnwrapOccurrence> {
    let func = node.child_by_field_name("function")?;
    if func.kind() != "field_expression" {
        return None;
    }

    let field = func.child_by_field_name("field")?;
    let method_name = field.utf8_text(source).ok()?;
    if method_name != "unwrap" && method_name != "expect" {
        return None;
    }

    let receiver = func
        .child_by_field_name("value")
        .or_else(|| func.child(0))
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or_default()
        .to_string();

    let args_node = node.child_by_field_name("arguments");
    let args_text = args_node
        .and_then(|a| a.utf8_text(source).ok())
        .unwrap_or_default();

    let start_pos = node.start_position();
    let snippet = node.utf8_text(source).unwrap_or_default().to_string();
    let context = enclosing_context(node, source);

    let (classification, reason) = classify_unwrap(
        rel_path,
        method_name,
        &receiver,
        args_text,
        &context,
        &snippet,
    );

    Some(UnwrapOccurrence {
        file: rel_path.to_string(),
        line: start_pos.row + 1,
        column: start_pos.column + 1,
        method: method_name.to_string(),
        snippet,
        receiver,
        context,
        classification,
        reason,
    })
}

fn enclosing_context(node: Node, source: &[u8]) -> String {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "function_item" {
            if let Some(name) = parent.child_by_field_name("name") {
                let fn_name = name.utf8_text(source).unwrap_or("fn");
                let ret = parent
                    .child_by_field_name("return_type")
                    .and_then(|r| r.utf8_text(source).ok())
                    .unwrap_or("");
                return format!("{fn_name}{ret}");
            }
            return "fn".to_string();
        }
        current = parent.parent();
    }
    "top_level".to_string()
}

fn is_lock_acquisition(rec_low: &str, args_low: &str) -> bool {
    rec_low.contains("lock()")
        || rec_low.contains("read()")
        || rec_low.contains("write()")
        || args_low.contains("poisoned")
        || args_low.contains("lock")
}

fn is_static_regex(rec_low: &str) -> bool {
    rec_low.contains("regex::new") || rec_low.contains("bytesregex::new")
}

fn is_proven_invariant(args_low: &str, snip_low: &str) -> bool {
    args_low.contains("static")
        || args_low.contains("checked above")
        || args_low.contains("always serializes")
        || args_low.contains("non-empty")
        || args_low.contains("statically valid")
        || args_low.contains("marker present")
        || args_low.contains("install sigterm")
        || args_low.contains("ctrl+c")
        || snip_low.contains("split_once(\"⟪here⟫\")")
}

fn is_local_failure_context(rel_path: &str, context: &str) -> bool {
    rel_path.contains("test_util")
        || rel_path.contains("mock.rs")
        || rel_path.contains("mcp-forge")
        || context.contains("-> Result")
        || context.contains("-> Option")
}

fn classify_unwrap(
    rel_path: &str,
    _method: &str,
    receiver: &str,
    args: &str,
    context: &str,
    snippet: &str,
) -> (Classification, String) {
    let rec_low = receiver.to_ascii_lowercase();
    let args_low = args.to_ascii_lowercase();
    let snip_low = snippet.to_ascii_lowercase();

    if is_lock_acquisition(&rec_low, &args_low) {
        return (
            Classification::ProvenInvariant,
            "mutex / rwlock acquisition on internal non-poisoned state".to_string(),
        );
    }

    if is_static_regex(&rec_low) {
        return (
            Classification::ProvenInvariant,
            "statically validated regex constant".to_string(),
        );
    }

    if is_proven_invariant(&args_low, &snip_low) {
        return (
            Classification::ProvenInvariant,
            "statically proven invariant or startup runtime boundary".to_string(),
        );
    }

    if is_local_failure_context(rel_path, context) {
        return (
            Classification::LocalFailure,
            "recoverable or scoped helper context".to_string(),
        );
    }

    (
        Classification::ProcessFatal,
        "production path unwrap requiring conversion to Result or waiver".to_string(),
    )
}

#[cfg(test)]
#[path = "classifier/regression_tests.rs"]
mod regression_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_source_filter_excludes_test_support_paths() {
        assert!(is_production_source("crates/sample/src/lib.rs"));
        assert!(is_production_source("crates/sample/src/runtime/worker.rs"));
        assert!(!is_production_source("README.md"));
        assert!(!is_production_source("crates/sample/tests/e2e.rs"));
        assert!(!is_production_source("crates/sample/src/tests.rs"));
        assert!(!is_production_source("crates/sample/src/runtime_tests.rs"));
        assert!(!is_production_source(
            "crates/sample/src/test_support/repo.rs"
        ));
        assert!(!is_production_source("crates/sample/src/examples/demo.rs"));
        assert!(!is_production_source("crates/sample/src/test_fixtures.rs"));
        assert!(!is_production_source("crates/sample/src/test_helpers.rs"));
        assert!(!is_production_source("crates/sample/src/test_util.rs"));
    }

    #[test]
    fn test_classify_simple_unwraps() {
        let code = r#"
            pub fn run() {
                let lock = mutex.lock().unwrap();
                let re = Regex::new("abc").unwrap();
                let fatal = user_input.parse::<u32>().unwrap();
            }

            #[cfg(test)]
            mod tests {
                #[test]
                fn test_ignored() {
                    let ignored = "123".parse::<u32>().unwrap();
                }
            }
        "#;

        let results = classify_file("crates/sample/src/lib.rs", code).unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].classification, Classification::ProvenInvariant);
        assert_eq!(results[1].classification, Classification::ProvenInvariant);
        assert_eq!(results[2].classification, Classification::ProcessFatal);
    }
}
