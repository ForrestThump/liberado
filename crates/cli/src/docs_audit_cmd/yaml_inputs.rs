use super::parse_example;

#[test]
fn yaml_check_fence_parses_a_document_and_rejects_broken_yaml() {
    let document = "\
kind: plan
status: active
open_items: true
supersedes:
  - old-plan.md
";
    parse_example("yaml", document).unwrap();
    parse_example("yaml", "- a\n- b\n").unwrap();
    let err = parse_example("yaml", "kind: [\n").unwrap_err();
    assert!(!err.to_string().is_empty());
}
