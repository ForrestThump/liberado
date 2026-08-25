use super::{Flags, parse_flags};

#[test]
fn parse_flags_takes_positional_and_flags() {
    let (positional, flags) = parse_flags(
        [
            "task.json".to_string(),
            "--endpoint".to_string(),
            "http://w:7780".to_string(),
        ]
        .into_iter(),
        "task.json path",
    )
    .expect("parses");
    assert_eq!(positional.as_deref(), Some("task.json"));
    assert_eq!(
        flags,
        Flags {
            endpoint: Some("http://w:7780".into()),
            token_env: None,
        }
    );
}

#[test]
fn parse_flags_rejects_unknown_and_dangling_flags() {
    let error = parse_flags(["--bogus".to_string()].into_iter(), "task-id")
        .expect_err("unknown flag must be an error, never a fall-through");
    assert!(error.contains("bogus"));

    let error = parse_flags(["--endpoint".to_string()].into_iter(), "task-id")
        .expect_err("dangling value must be an error");
    assert!(error.contains("endpoint"));
}

#[test]
fn a_second_positional_is_rejected() {
    let error = parse_flags(["a".to_string(), "b".to_string()].into_iter(), "task-id")
        .expect_err("two positionals is a usage error");
    assert!(error.contains("exactly one"));
}
