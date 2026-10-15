use super::{Value, meta_bool, meta_str, split_frontmatter};

#[test]
fn split_frontmatter_reads_managed_document_yaml() {
    let text = "\
---
kind: policy
status: active
authority: normative
domain: docs
canonical_for: document-authority
open_items: false
last_verified: 2026-09-22
supersedes:
  - old-plan.md
  - older-plan.md
---
# Title
";
    let (meta, body) = split_frontmatter(text);
    let meta = meta.expect("mapping");
    assert_eq!(meta_str(&meta, "kind"), Some("policy"));
    assert_eq!(meta_str(&meta, "status"), Some("active"));
    assert_eq!(meta_str(&meta, "authority"), Some("normative"));
    assert_eq!(meta_str(&meta, "domain"), Some("docs"));
    assert_eq!(meta_str(&meta, "canonical_for"), Some("document-authority"));
    assert_eq!(meta_str(&meta, "last_verified"), Some("2026-09-22"));
    assert_eq!(meta_bool(&meta, "open_items"), Some(false));
    assert_eq!(
        meta_str(&meta, "supersedes"),
        None,
        "a list is not a string"
    );
    let items: Vec<_> = meta
        .get(Value::String("supersedes".into()))
        .and_then(Value::as_sequence)
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(items, ["old-plan.md", "older-plan.md"]);
    assert_eq!(body, "# Title\n");
}

#[test]
fn invalid_frontmatter_yaml_drops_meta_and_keeps_the_body() {
    let (meta, body) = split_frontmatter("---\nkind: [\n---\n# Kept\n");
    assert!(meta.is_none());
    assert_eq!(body, "# Kept\n");
}

#[test]
fn a_yaml_list_is_not_document_meta() {
    let (meta, body) = split_frontmatter("---\n- just\n- a\n- list\n---\nbody\n");
    assert!(meta.is_none());
    assert_eq!(body, "body\n");
}

#[test]
fn crlf_frontmatter_normalizes_before_parse() {
    let (meta, body) =
        split_frontmatter("---\r\nkind: plan\r\nopen_items: true\r\n---\r\n# Hi\r\n");
    let meta = meta.expect("mapping");
    assert_eq!(meta_str(&meta, "kind"), Some("plan"));
    assert_eq!(meta_bool(&meta, "open_items"), Some(true));
    assert_eq!(body, "# Hi\n");
}
