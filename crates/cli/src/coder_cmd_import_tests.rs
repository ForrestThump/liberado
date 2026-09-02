//! Split from `coder_cmd.rs` for module-health boundaries.

use super::cmd_import;
use std::fs;

#[test]
fn import_requires_a_value_after_flag() {
    let cases = [
        ("-o", "-o requires a value"),
        ("--format", "--format requires kilo|openhands|auto"),
        ("--session-id", "--session-id requires a value"),
    ];
    for (flag, needle) in cases {
        let mut args = vec![flag.to_owned()].into_iter();
        let err = cmd_import(&mut args).unwrap_err().to_string();
        assert!(err.contains(needle), "{flag}: {err}");
    }
}

#[test]
fn import_rejects_two_positionals() {
    let mut args = vec!["a.json".to_owned(), "b.json".to_owned()].into_iter();
    let err = cmd_import(&mut args).unwrap_err().to_string();
    assert!(err.contains("takes a single input path"), "{err}");
}

#[test]
fn import_accepts_each_format_alias_before_reading_the_file() {
    for format in ["kilo", "kilo-cli", "kilocli", "openhands", "oh", "auto"] {
        let mut args = vec![
            "--format".to_owned(),
            format.to_owned(),
            "missing.json".to_owned(),
        ]
        .into_iter();
        let err = cmd_import(&mut args).unwrap_err().to_string();
        assert!(
            !err.contains("unknown --format"),
            "{format} must parse, got {err}"
        );
    }
}

#[test]
fn import_writes_a_kilo_messages_export() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("foreign.json");
    fs::write(&input, r#"[{"role":"user","content":"hello"}]"#).unwrap();
    let output = dir.path().join("out.messages.json");
    let mut args = vec![
        input.to_string_lossy().into_owned(),
        "-o".to_owned(),
        output.to_string_lossy().into_owned(),
        "--format".to_owned(),
        "kilo".to_owned(),
        "--session-id".to_owned(),
        "imported-1".to_owned(),
    ]
    .into_iter();
    cmd_import(&mut args).expect("a kilo user-message list must import");
    let body = fs::read_to_string(&output).unwrap();
    assert!(body.contains("hello"), "{body}");
    assert!(body.contains("imported-1"), "{body}");
}
