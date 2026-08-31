use super::*;
use tempfile::tempdir;

#[test]
fn checked_example_parser_covers_all_supported_languages() {
    parse_example("toml", "value = 1").unwrap();
    parse_example("json", r#"{"value":1}"#).unwrap();
    parse_example("yaml", "value: 1").unwrap();
    assert!(parse_example("json", "{").is_err());
}

#[test]
fn markdown_collection_recurses_and_ignores_other_extensions() {
    let root = tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("nested")).unwrap();
    std::fs::write(root.path().join("root.md"), "root").unwrap();
    std::fs::write(root.path().join("nested/page.md"), "page").unwrap();
    std::fs::write(root.path().join("nested/data.json"), "{}").unwrap();
    let mut paths = Vec::new();
    collect_markdown(root.path(), &mut paths).unwrap();
    paths.sort();
    assert_eq!(paths.len(), 2);
    assert!(paths.iter().all(|path| path.extension().unwrap() == "md"));
}

#[test]
fn impact_policy_validation_rejects_duplicates_and_empty_rules() {
    let root = tempdir().unwrap();
    std::fs::write(root.path().join("contract.md"), "contract").unwrap();
    let valid = || Impact {
        name: "config".into(),
        sources: vec!["src/config/".into()],
        documents: vec!["contract.md".into()],
    };
    let rule = valid();
    validate_impacts(root.path(), std::slice::from_ref(&rule)).unwrap();
    assert!(validate_impacts(root.path(), &[valid(), valid()]).is_err());
    assert!(
        validate_impacts(
            root.path(),
            &[Impact {
                name: "empty".into(),
                sources: Vec::new(),
                documents: vec!["contract.md".into()],
            }]
        )
        .is_err()
    );
}
