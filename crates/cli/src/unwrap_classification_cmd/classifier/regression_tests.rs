use super::*;
use tempfile::tempdir;

#[test]
fn analyze_tree_walks_production_rust_and_skips_test_directories() {
    let root = tempdir().unwrap();
    let source = root.path().join("crates/sample/src");
    std::fs::create_dir_all(source.join("nested")).unwrap();
    std::fs::create_dir_all(root.path().join("crates/sample/tests")).unwrap();
    std::fs::write(
        source.join("lib.rs"),
        "pub fn parse(input: &str) { let _ = input.parse::<u32>().unwrap(); }",
    )
    .unwrap();
    std::fs::write(source.join("nested/mod.rs"), "pub fn clean() {}").unwrap();
    std::fs::write(source.join("notes.txt"), "not Rust").unwrap();
    std::fs::write(
        root.path().join("crates/sample/tests/integration.rs"),
        "pub fn ignored() { panic!(\"not production\"); }",
    )
    .unwrap();

    let report = analyze_tree(root.path()).unwrap();

    assert_eq!(report.summary.files_scanned, 2);
    assert_eq!(report.summary.files_with_unwraps, 1);
    assert_eq!(report.summary.total_unwraps, 1);
    assert_eq!(report.summary.process_fatal, 1);
    assert!(report.files.contains_key("crates/sample/src/lib.rs"));
    assert!(
        !report
            .files
            .contains_key("crates/sample/tests/integration.rs")
    );
}
