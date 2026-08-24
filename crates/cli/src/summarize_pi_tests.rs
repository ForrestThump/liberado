//! Split from `summarize_cmd.rs`: the pi-ingest renderer tests.

#![allow(unused_imports)]

use super::*;
use tempfile::tempdir;

#[test]
fn summarize_pi_renders_turns_tools_edits_and_timeouts() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    fs::write(
        &path,
        concat!(
            "{\"type\":\"turn_start\"}\n",
            "{\"type\":\"tool_execution_start\",\"toolName\":\"bash\",\"args\":{\"command\":\"cargo test --workspace\"}}\n",
            "{\"type\":\"tool_execution_start\",\"toolName\":\"read_file\"}\n",
            "{\"type\":\"turn_start\"}\n",
            "{\"type\":\"tool_execution_start\",\"name\":\"edit\",\"input\":{\"path\":\"src/x.rs\"}}\n",
            "{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"first \"},{\"type\":\"other\"},{\"type\":\"text\",\"text\":\"part\"}]}}\n",
            "{\"type\":\"turn_start\"}\n",
            "{\"type\":\"tool_execution_start\",\"toolName\":\"write\",\"args\":{\"path\":\"src/second.rs\"}}\n",
            "{\"type\":\"noise\",\"detail\":\"saw a Connect Timeout while streaming\"}\n"
        ),
    )
    .unwrap();

    let out = summarize_pi(&path).unwrap();
    assert!(out.contains("## pi  session.jsonl"), "{out}");
    assert!(
        out.contains("- turns: 3   tools: {bash: 1, edit: 1, read_file: 1, write: 1}"),
        "{out}"
    );
    assert!(
        out.contains("- first edit: (2, src/x.rs)"),
        "the FIRST edit/write call pins turn and path; a later write must not win, got: {out}"
    );
    assert!(out.contains("- connect-timeout mentions: 1"), "{out}");
    assert!(out.contains("t1 cargo test --workspace"), "{out}");
    assert!(
        out.contains("- last assistant: first part"),
        "text blocks join and non-text blocks drop, got: {out}"
    );
}
#[test]
fn summarize_pi_omits_empty_sections() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    fs::write(&path, "{\"type\":\"turn_start\"}\n").unwrap();
    let out = summarize_pi(&path).unwrap();
    assert!(!out.contains("- cargo:"), "{out}");
    assert!(!out.contains("- last assistant:"), "{out}");
    assert!(out.contains("- first edit: None"), "{out}");
}
#[test]
fn mvl_and_pi_tolerate_empty_and_partial_input() {
    let dir = tempdir().unwrap();
    let mvl_path = dir.path().join("run.mvl.jsonl");
    fs::write(&mvl_path, "").unwrap();
    assert!(mvl(&mvl_path, true).is_ok(), "empty MVL must not fail");
    let pi_path = dir.path().join("session.jsonl");
    fs::write(
        &pi_path,
        "{\"type\":\"turn_start\"}\nnot-json\n{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n",
    )
    .unwrap();
    assert!(pi(&pi_path).is_ok(), "partial pi session must not fail");
}
#[test]
fn kind_classifies_known_shapes() {
    let dir = tempdir().unwrap();
    // A directory with any .json file is a liberado traces dir.
    fs::write(dir.path().join("x.json"), "{}").unwrap();
    assert_eq!(kind(dir.path()), "liberado-dir");
    // An empty directory is just a directory.
    let empty = tempdir().unwrap();
    assert_eq!(kind(empty.path()), "dir");
    // Bare .json / .jsonl files by extension.
    let json_path = dir.path().join("traces.json");
    fs::write(&json_path, "{}").unwrap();
    assert_eq!(kind(&json_path), "liberado-json");
    let jsonl_path = dir.path().join("x.jsonl");
    fs::write(&jsonl_path, "{}").unwrap();
    assert_eq!(kind(&jsonl_path), "jsonl");
    // Unknown extension.
    let unknown = dir.path().join("notes.txt");
    fs::write(&unknown, "x").unwrap();
    assert_eq!(kind(&unknown), "unknown");
}

#[test]
fn kind_treats_a_pi_only_directory_as_a_compare_layout() {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("pi")).unwrap();
    assert_eq!(kind(dir.path()), "compare");
}
