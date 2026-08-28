//! Split from `lib.rs` for module-health boundaries.

use super::*;
use crate::{CodingToolRuntime, ToolError};
use liberado_coder_core::{CommandPolicy, HashlineConfig, PathPolicy};
use serde_json::json;

/// A runtime with hashline **explicitly off**.
///
/// These tests predate hashline mode and assert `read_file`'s plain output. They used to
/// inherit that from `HashlineConfig::default()`, which then flipped to enabled — so five of
/// them broke at once for a reason none of them mentioned. Pinning it here makes each test
/// state the mode it is testing instead of borrowing a global decision that can move.
fn runtime() -> (tempfile::TempDir, CodingToolRuntime) {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
            .unwrap()
            .with_hashline(HashlineConfig {
                enabled: false,
                hash_length: HashlineConfig::HASH_LENGTH_MIN,
            });
    (dir, runtime)
}

#[tokio::test]
async fn read_file_caps_large_content() {
    let (dir, mut runtime) = runtime();
    runtime.path_policy.read_max_bytes = 4;
    std::fs::write(dir.path().join("big.txt"), "abcdef").unwrap();

    let result = runtime
        .invoke_json("read_file", json!({"path": "big.txt"}))
        .await
        .unwrap();

    assert_eq!(result["content"], "abcd");
}

#[tokio::test]
async fn denied_path_is_not_read() {
    let (dir, runtime) = runtime();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::write(dir.path().join(".git/config"), "secret").unwrap();
    let err = runtime
        .invoke_json("read_file", json!({"path": ".git/config"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::PathDenied(_)));
}

#[tokio::test]
async fn grep_returns_line_matches() {
    let (dir, runtime) = runtime();
    std::fs::write(
        dir.path().join("notes.txt"),
        "alpha
beta
",
    )
    .unwrap();
    let result = runtime
        .invoke_json("grep", json!({"pattern": "beta", "output_mode": "content"}))
        .await
        .unwrap();
    assert_eq!(result["matches"][0]["path"], "notes.txt");
    assert_eq!(result["matches"][0]["line"], 2);
}

#[tokio::test]
async fn grep_on_a_file_path_searches_that_file() {
    let (dir, runtime) = runtime();
    std::fs::write(dir.path().join("notes.txt"), "alpha\nbeta\n").unwrap();
    let result = runtime
        .invoke_json(
            "grep",
            json!({"pattern": "beta", "path": "notes.txt", "output_mode": "content"}),
        )
        .await
        .expect("grep of a file must not fail as an invalid directory");
    assert_eq!(result["total"], 1);
    assert_eq!(result["matches"][0]["path"], "notes.txt");
}

// ── grep ─────────────────────────────────────────────────────────────────────────────

/// The old `search_text` is still callable. A run started against an older catalog must not
/// fail on a rename it never saw.
#[tokio::test]
async fn search_text_is_still_accepted_as_an_alias() {
    let (dir, runtime) = runtime();
    std::fs::write(dir.path().join("notes.txt"), "alpha\nbeta\n").unwrap();
    let result = runtime
        .invoke_json("search_text", json!({"pattern": "beta"}))
        .await
        .unwrap();
    assert_eq!(result["total"], 1);
}

/// Regex, not `contains`. The predecessor could only match literal text, which is why a
/// question like "every function starting with handle_" could not be asked at all.
#[tokio::test]
async fn a_regex_pattern_matches() {
    let (dir, runtime) = runtime();
    std::fs::write(
        dir.path().join("a.rs"),
        "fn handle_one() {}\nfn other() {}\nfn handle_two() {}\n",
    )
    .unwrap();
    let out = runtime
        .invoke_json(
            "grep",
            json!({"pattern": r"fn handle_\w+", "output_mode": "content"}),
        )
        .await
        .unwrap();
    assert_eq!(out["total"], 2, "{out}");
}

/// A literal search must still be possible without escaping a regex by hand.
#[tokio::test]
async fn fixed_strings_searches_literally() {
    let (dir, runtime) = runtime();
    std::fs::write(dir.path().join("a.rs"), "let v: Vec<u8> = vec![];\n").unwrap();
    let out = runtime
        .invoke_json(
            "grep",
            json!({"pattern": "Vec<u8>", "fixed_strings": true, "output_mode": "content"}),
        )
        .await
        .unwrap();
    assert_eq!(out["total"], 1, "{out}");
}

/// A rejected pattern must say what to do next. "Invalid regex" alone gets the same pattern
/// back with more escapes — the same shape as the anchor errors that cost earlier runs.
#[tokio::test]
async fn an_invalid_regex_names_the_way_out() {
    let (_dir, runtime) = runtime();
    let err = runtime
        .invoke_json("grep", json!({"pattern": "fn foo("}))
        .await
        .expect_err("an unbalanced paren is not a valid regex");
    let m = err.to_string();
    assert!(
        m.contains("fixed_strings"),
        "must offer the literal escape hatch: {m}"
    );
}

#[tokio::test]
async fn case_insensitive_search_works() {
    let (dir, runtime) = runtime();
    std::fs::write(dir.path().join("a.rs"), "TODO: fix\n").unwrap();
    let out = runtime
        .invoke_json("grep", json!({"pattern": "todo", "-i": true}))
        .await
        .unwrap();
    assert_eq!(out["total"], 1, "{out}");
}

/// The default mode lists paths, which is what "where does this live" needs. Content mode is
/// opt-in because returning every matching line by default buries the answer.
#[tokio::test]
async fn the_default_mode_lists_files_not_lines() {
    let (dir, runtime) = runtime();
    std::fs::write(dir.path().join("a.rs"), "x\nx\nx\n").unwrap();
    let out = runtime
        .invoke_json("grep", json!({"pattern": "x"}))
        .await
        .unwrap();
    assert_eq!(out["files"][0], "a.rs");
    assert_eq!(out["total"], 3, "the count is still reported: {out}");
    assert!(
        out["matches"].is_null(),
        "lines must not be returned by default: {out}"
    );
}

#[tokio::test]
async fn content_mode_can_include_surrounding_lines() {
    let (dir, runtime) = runtime();
    std::fs::write(dir.path().join("a.rs"), "one\ntwo\nTARGET\nfour\nfive\n").unwrap();
    let out = runtime
        .invoke_json(
            "grep",
            json!({"pattern": "TARGET", "output_mode": "content", "-C": 1}),
        )
        .await
        .unwrap();
    let ctx = out["matches"][0]["context"].as_str().unwrap_or_default();
    assert_eq!(ctx, "two\nTARGET\nfour", "{out}");
}

/// The glob filters on the basename. A path-anchored pattern matching nothing silently is a
/// trap the description warns about, and the behaviour has to match the warning.
#[tokio::test]
async fn a_glob_filters_by_file_name() {
    let (dir, runtime) = runtime();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "needle\n").unwrap();
    std::fs::write(dir.path().join("src/b.txt"), "needle\n").unwrap();

    let rs = runtime
        .invoke_json("grep", json!({"pattern": "needle", "glob": "*.rs"}))
        .await
        .unwrap();
    assert_eq!(rs["total"], 1, "only the .rs file should match: {rs}");
}

/// The finding that motivated the fallback: an empty result cannot be told apart from a
/// wrong pattern, and a model that cannot tell them apart edits from memory instead.
#[tokio::test]
async fn a_pattern_that_matches_nothing_suggests_what_was_close() {
    let (dir, runtime) = runtime();
    std::fs::write(
        dir.path().join("a.rs"),
        "let Some(observer) = self.observer.as_ref() else { return };\n",
    )
    .unwrap();

    // The exact anchor a real run invented: plural field, and a loop that does not exist.
    let out = runtime
        .invoke_json(
            "grep",
            json!({"pattern": "for observer in &self.observers"}),
        )
        .await
        .unwrap();
    assert_eq!(out["total"], 0);
    let suggestions = out["did_you_mean"].as_array().cloned().unwrap_or_default();
    assert!(
        !suggestions.is_empty(),
        "a miss must point at the nearest real line: {out}"
    );
    assert!(
        suggestions[0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("self.observer"),
        "the suggestion must be the line that actually exists: {out}"
    );
}

/// And a miss with nothing remotely similar says so plainly rather than inventing a lead.
#[tokio::test]
async fn a_miss_with_no_near_lines_suggests_nothing() {
    let (dir, runtime) = runtime();
    std::fs::write(dir.path().join("a.rs"), "x\n").unwrap();
    let out = runtime
        .invoke_json(
            "grep",
            json!({"pattern": "zzzzz_completely_unrelated_identifier_qqqq"}),
        )
        .await
        .unwrap();
    assert_eq!(out["total"], 0);
    assert!(
        out["did_you_mean"]
            .as_array()
            .map(Vec::is_empty)
            .unwrap_or(true),
        "a bad guess is worse than no guess: {out}"
    );
}

#[tokio::test]
async fn count_mode_reports_per_file_totals() {
    let (dir, runtime) = runtime();
    std::fs::write(dir.path().join("a.rs"), "hit\nhit\n").unwrap();
    let out = runtime
        .invoke_json("grep", json!({"pattern": "hit", "output_mode": "count"}))
        .await
        .unwrap();
    assert_eq!(out["counts"][0]["count"], 2, "{out}");
    assert_eq!(out["total"], 2);
}

#[tokio::test]
async fn an_empty_pattern_is_rejected() {
    let (_dir, runtime) = runtime();
    runtime
        .invoke_json("grep", json!({"pattern": ""}))
        .await
        .expect_err("an empty pattern would match every line in the tree");
}

#[tokio::test]
async fn list_files_returns_workspace_contents() {
    let (dir, runtime) = runtime();
    std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();

    let result = runtime.invoke_json("list_files", json!({})).await.unwrap();

    let files = result["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(files.contains(&"a.txt".to_string()), "should contain a.txt");
    assert!(files.contains(&"b.txt".to_string()), "should contain b.txt");
}

#[tokio::test]
async fn list_files_respects_limit() {
    let (dir, runtime) = runtime();
    for i in 0..10 {
        std::fs::write(dir.path().join(format!("f{i}.txt")), "x\n").unwrap();
    }

    let result = runtime
        .invoke_json("list_files", json!({"limit": 3}))
        .await
        .unwrap();

    assert_eq!(result["limit"], 3);
    let files = result["files"].as_array().unwrap();
    assert!(files.len() <= 3, "limit 3 should cap results");
}

#[tokio::test]
/// The old name with the old parameter names, kept as a regression on the alias: a run that
/// started against the previous catalog calls `search_text(query, limit)` and must still work.
async fn the_old_search_text_call_shape_still_works() {
    let (dir, runtime) = runtime();
    std::fs::write(
        dir.path().join("notes.txt"),
        "alpha
alpha
beta
",
    )
    .unwrap();

    let result = runtime
        .invoke_json(
            "search_text",
            json!({"query": "alpha", "limit": 1, "output_mode": "content"}),
        )
        .await
        .unwrap();

    assert_eq!(result["matches"].as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn walk_files_respects_limit() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..10 {
        std::fs::write(dir.path().join(format!("f{i}.txt")), "x\n").unwrap();
    }

    let mut count = 0usize;
    walk_files(dir.path(), 3, &PathPolicy::default(), |_, _| {
        count += 1;
        true
    })
    .unwrap();

    assert_eq!(count, 3, "walk_files should visit at most 3 files");
}

#[tokio::test]
async fn a_denied_directory_neither_counts_against_the_limit_nor_is_descended_into() {
    // The budget is what makes this matter. `.git` alone is thousands of files in a real
    // checkout, so a walk that counts them first and filters them second returns nothing the
    // caller can use — which is what `list_symbols` did against this repo.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    for i in 0..20 {
        std::fs::write(dir.path().join(".git").join(format!("o{i}")), "x\n").unwrap();
    }
    std::fs::write(dir.path().join("real.rs"), "pub fn kept() {}\n").unwrap();

    let mut seen = Vec::new();
    walk_files(dir.path(), 5, &PathPolicy::default(), |_, rel| {
        seen.push(rel.to_string());
        true
    })
    .unwrap();

    assert_eq!(
        seen,
        vec!["real.rs".to_string()],
        "a denied directory must not spend the caller's budget"
    );
}

#[test]
fn extract_rust_visibility_variants() {
    let content = "pub(crate) fn internal() {}\npub(super) fn semi_public() {}\npub fn public() {}\nfn private() {}";
    let symbols = extract_symbols("src/lib.rs", content);
    assert!(symbols.contains(&"fn internal".to_string()));
    assert!(symbols.contains(&"fn semi_public".to_string()));
    assert!(symbols.contains(&"fn public".to_string()));
    assert!(symbols.contains(&"fn private".to_string()));
}

#[test]
fn extract_rust_const_and_unsafe_fn() {
    let content = "const fn compile_time() -> u32 { 42 }\npub const fn pub_compile() {}\nunsafe fn raw_op() {}\npub unsafe fn pub_raw() {}";
    let symbols = extract_symbols("src/lib.rs", content);
    assert!(symbols.contains(&"fn compile_time".to_string()));
    assert!(symbols.contains(&"fn pub_compile".to_string()));
    assert!(symbols.contains(&"fn raw_op".to_string()));
    assert!(symbols.contains(&"fn pub_raw".to_string()));
}

#[test]
fn extract_rust_attribute_prefixed() {
    let content = "#[derive(Debug)]\npub struct Foo;\n\n#[inline]\npub fn bar() {}";
    let symbols = extract_symbols("src/lib.rs", content);
    assert!(symbols.contains(&"struct Foo".to_string()));
    assert!(symbols.contains(&"fn bar".to_string()));
}

#[test]
fn extract_rust_generic_impl() {
    let content = "impl<T> MyType<T> {\n    fn new() -> Self {}\n}\n\nimpl PlainType {\n    fn method() {}\n}";
    let symbols = extract_symbols("src/lib.rs", content);
    assert!(
        symbols.iter().any(|s| s.contains("MyType")),
        "should find generic impl"
    );
    assert!(symbols.contains(&"impl PlainType".to_string()));
}

#[test]
fn extract_ts_export_default_function() {
    let content = "export default function main() {}\nexport function helper() {}";
    let symbols = extract_symbols("app.ts", content);
    assert!(symbols.contains(&"export function main".to_string()));
    assert!(symbols.contains(&"export function helper".to_string()));
}

#[test]
fn extract_rust_traits_and_enums() {
    let content =
        "pub trait Handler {\n    fn handle(&self);\n}\n\npub enum Status {\n    Ok,\n    Err,\n}";
    let symbols = extract_symbols("src/types.rs", content);
    assert!(symbols.contains(&"trait Handler".to_string()));
    assert!(symbols.contains(&"enum Status".to_string()));
}

#[test]
fn extract_rust_mods_and_impls() {
    let content = "pub mod sub;\nmod private_mod {\n}\n\nimpl MyType {\n    fn new() -> Self {}\n}";
    let symbols = extract_symbols("src/lib.rs", content);
    assert!(symbols.contains(&"mod sub".to_string()));
    assert!(symbols.contains(&"mod private_mod".to_string()));
    assert!(symbols.contains(&"impl MyType".to_string()));
}

#[test]
fn extract_python_symbols() {
    let content =
        "def handle_request(req):\n    pass\n\nclass Router:\n    def route(self):\n        pass";
    let symbols = extract_symbols("app.py", content);
    assert!(symbols.contains(&"def handle_request".to_string()));
    assert!(symbols.contains(&"class Router".to_string()));
    // Indentation is Python's nesting. `route` is a method on `Router`; listing it beside the
    // module-level names says the module exports something it does not. The guard for this
    // was already written — it just never saw an indented line to reject.
    assert!(
        !symbols.contains(&"def route".to_string()),
        "a nested def is not a top-level symbol: {symbols:?}"
    );
}

#[test]
fn extract_typescript_symbols() {
    let content = "export function main() {}\n\nclass App {\n    render() {}\n}\n\ninterface Config {\n    port: number;\n}\n\nconst handler = (req: Request) => { return 200; };";
    let symbols = extract_symbols("app.ts", content);
    assert!(symbols.contains(&"export function main".to_string()));
    assert!(symbols.contains(&"class App".to_string()));
    assert!(symbols.contains(&"interface Config".to_string()));
    assert!(symbols.contains(&"const handler".to_string()));
}

#[test]
fn extract_go_symbols() {
    let content = "func main() {\n}\n\ntype Server struct {\n    port int\n}\n\ntype Handler interface {\n    Serve()\n}";
    let symbols = extract_symbols("main.go", content);
    assert!(symbols.contains(&"func main".to_string()));
    assert!(symbols.contains(&"type Server struct".to_string()));
    assert!(symbols.contains(&"type Handler interface".to_string()));
}

#[test]
fn extract_java_symbols() {
    let content = "public class Main {\n    public static void main(String[] args) {}\n}\n\npublic enum State {\n    ON, OFF\n}\n\npublic interface Runnable {\n    void run();\n}";
    let symbols = extract_symbols("Main.java", content);
    assert!(symbols.contains(&"class Main".to_string()));
    assert!(symbols.contains(&"enum State".to_string()));
    assert!(symbols.contains(&"interface Runnable".to_string()));
}

#[test]
fn unknown_extension_returns_empty() {
    let content = "fn main() {}";
    let symbols = extract_symbols("README.md", content);
    assert!(symbols.is_empty());
}

#[test]
fn skips_comments() {
    let content = "// fn not_a_fn() {}\n/* struct Fake {} */\n\nfn real() {}";
    let symbols = extract_symbols("src/lib.rs", content);
    assert_eq!(symbols.len(), 1);
    assert!(symbols.contains(&"fn real".to_string()));
}

#[tokio::test]
async fn list_symbols_returns_workspace_symbols() {
    let (dir, runtime) = runtime();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn answer() -> u8 { 42 }\n\npub struct Config { pub port: u16 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("README.md"),
        "# Project\nfn not_extracted() {}\n",
    )
    .unwrap();

    let result = runtime
        .invoke_json("list_symbols", json!({}))
        .await
        .unwrap();
    let files = result["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["path"], "src/lib.rs");
    let symbols: Vec<String> = files[0]["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect();
    assert!(symbols.contains(&"fn answer".to_string()));
    assert!(symbols.contains(&"struct Config".to_string()));
}

#[tokio::test]
async fn list_symbols_respects_limit() {
    let (dir, runtime) = runtime();
    for i in 0..5 {
        std::fs::write(
            dir.path().join(format!("f{i}.rs")),
            format!("fn f{i}() {{}}"),
        )
        .unwrap();
    }

    let result = runtime
        .invoke_json("list_symbols", json!({"limit": 2}))
        .await
        .unwrap();

    let files = result["files"].as_array().unwrap();
    assert!(files.len() <= 2, "limit 2 should cap results");
    assert_eq!(result["limit"], 2);
}

#[tokio::test]
async fn list_symbols_limit_bounds_what_is_returned_not_what_is_walked() {
    // A source tree is mostly not source. When `limit` doubled as the walk budget, the
    // non-source files ahead of the code in breadth-first order consumed it — against this
    // repo the tool returned exactly one file, a stray `validate.py`, and no Rust at all.
    let (dir, runtime) = runtime();
    for i in 0..10 {
        std::fs::write(
            dir.path().join(format!("a{i}.md")),
            "# not source
",
        )
        .unwrap();
    }
    for i in 0..3 {
        std::fs::write(
            dir.path().join(format!("z{i}.rs")),
            format!(
                "pub fn thing{i}() {{}}
"
            ),
        )
        .unwrap();
    }

    let result = runtime
        .invoke_json("list_symbols", json!({"limit": 2}))
        .await
        .unwrap();

    let files = result["files"].as_array().unwrap();
    assert_eq!(
        files.len(),
        2,
        "limit is a result bound, and there are 3 source files past 10 non-source ones: {files:?}"
    );
    for f in files {
        let path = f["path"].as_str().unwrap();
        assert!(
            path.ends_with(".rs"),
            "only source files carry symbols: {path}"
        );
    }
}

#[tokio::test]
async fn list_symbols_stops_reading_a_file_at_the_policy_cap() {
    // `read_file` caps at `read_max_bytes`; this walked every file with an uncapped
    // `read_to_string`, which on a binary reads the whole thing before failing to decode it.
    let dir = tempfile::tempdir().unwrap();
    let policy = PathPolicy {
        read_max_bytes: 64,
        ..PathPolicy::default()
    };
    let runtime = CodingToolRuntime::new(dir.path(), CommandPolicy::default(), policy).unwrap();
    let mut content = String::from(
        "pub fn early() {}
",
    );
    content.push_str(
        &"// padding padding padding
"
        .repeat(20),
    );
    content.push_str(
        "pub fn beyond_the_cap() {}
",
    );
    std::fs::write(dir.path().join("big.rs"), &content).unwrap();

    let result = runtime
        .invoke_json("list_symbols", json!({}))
        .await
        .unwrap();

    let symbols = result["files"][0]["symbols"].as_array().unwrap();
    assert!(
        symbols.iter().any(|s| s == "fn early"),
        "the head of the file is still read: {symbols:?}"
    );
    assert!(
        !symbols.iter().any(|s| s == "fn beyond_the_cap"),
        "nothing past read_max_bytes should be reachable: {symbols:?}"
    );
}

#[tokio::test]
async fn grep_rejects_an_empty_query_under_either_name() {
    let (_dir, runtime) = runtime();
    runtime
        .invoke_json("search_text", json!({"query": ""}))
        .await
        .expect_err("an empty pattern would match every line in the tree");
}

// ── Java symbol extraction ────────────────────────────────────────

#[test]
fn extract_java_non_public_class_interface_enum() {
    assert_eq!(
        extract_java_symbol("class PackagePrivate { }"),
        Some("class PackagePrivate".to_string())
    );
    assert_eq!(
        extract_java_symbol("interface Contract { }"),
        Some("interface Contract".to_string())
    );
    assert_eq!(
        extract_java_symbol("enum Color { RED, GREEN }"),
        Some("enum Color".to_string())
    );
}

#[test]
fn extract_ts_plain_and_exported_functions() {
    assert_eq!(
        extract_ts_symbol("function foo(a) {"),
        Some("function foo".into())
    );
    assert_eq!(
        extract_ts_symbol("export function bar(a) {"),
        Some("export function bar".into())
    );
    assert_eq!(
        extract_ts_symbol("export default async function baz(a) {"),
        Some("export function baz".into()),
    );
}

#[test]
fn extract_ts_classes_interfaces_and_consts() {
    assert_eq!(extract_ts_symbol("class Foo {"), Some("class Foo".into()));
    assert_eq!(
        extract_ts_symbol("export abstract class Bar implements X {"),
        Some("class Bar".into()),
    );
    assert_eq!(
        extract_ts_symbol("interface IFoo {"),
        Some("interface IFoo".into())
    );
    assert_eq!(
        extract_ts_symbol("export interface IFoo {"),
        Some("interface IFoo".into()),
    );
    assert_eq!(
        extract_ts_symbol("const pick = (a) => a[0]"),
        Some("const pick".into()),
    );
}

#[test]
fn extract_ts_ignores_comments_and_non_declarations() {
    assert_eq!(extract_ts_symbol("// comment"), None);
    assert_eq!(extract_ts_symbol("const plain = 42"), None);
    assert_eq!(extract_ts_symbol("not a symbol"), None);
}
