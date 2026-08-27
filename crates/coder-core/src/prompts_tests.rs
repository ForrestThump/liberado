//! Split from `prompts.rs` for module-health boundaries.

use super::*;

/// The baked copies must not be empty. `include_str!` of a path that resolves to an empty
/// file compiles happily and produces a model call with no instructions.
#[test]
fn every_baked_prompt_has_content() {
    for (name, text) in [
        ("coder", CODER),
        ("diff-reviewer", DIFF_REVIEWER),
        ("cold-pr-reviewer", COLD_PR_REVIEWER),
        ("session-critic", SESSION_CRITIC),
        ("session-pack-coder", SESSION_PACK_CODER),
        ("intake", INTAKE),
        ("interactive", INTERACTIVE),
    ] {
        assert!(
            text.trim().len() > 200,
            "{name} baked prompt is {} chars; a prompt this short is a build accident",
            text.trim().len()
        );
    }
}

/// Every baked prompt must come from a file that is still there, or the on-disk override
/// silently stops working while the binary keeps using a snapshot nobody can find.
#[test]
fn every_baked_prompt_has_a_file_on_disk() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root");
    for file in [
        CODER_FILE,
        DIFF_REVIEWER_FILE,
        COLD_PR_REVIEWER_FILE,
        SESSION_CRITIC_FILE,
        SESSION_PACK_CODER_FILE,
        INTAKE_FILE,
        INTERACTIVE_FILE,
    ] {
        let path = root.join(PROMPT_DIR).join(file);
        assert!(
            path.is_file(),
            "{} is baked in but missing from disk; the override path is dead",
            path.display()
        );
    }
}

/// Interactive ACP coding must not tell the model to `submit_report`: that tool is the
/// one-shot pack's terminator, and offering the instruction without the tool is how a
/// conversation tries to file a report it cannot file.
#[test]
fn interactive_prompt_does_not_offer_submit_report() {
    let text = INTERACTIVE.to_ascii_lowercase();
    assert!(
        text.contains("do **not** have `submit_report`")
            || text.contains("do not have `submit_report`")
            || text.contains("you do **not** have `submit_report`"),
        "interactive.md must tell the model it has no submit_report; got {} chars",
        INTERACTIVE.len()
    );
    assert!(
        !INTERACTIVE.contains("then submit_report"),
        "must not instruct the model to call submit_report"
    );
    assert!(
        text.contains("`done`"),
        "interactive.md must tell the model about `done` when the project configured checks"
    );
}

#[test]
fn a_file_on_disk_wins_over_the_baked_copy() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("coder.md"), "OVERRIDDEN").expect("write");
    assert_eq!(load(Some(dir.path()), "coder.md", CODER), "OVERRIDDEN");
}

#[test]
fn an_absent_file_falls_back_to_the_baked_copy() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(load(Some(dir.path()), "coder.md", CODER), CODER);
}

/// An empty prompt file is an accident, not an instruction to run with no prompt.
#[test]
fn an_empty_file_falls_back_rather_than_blanking_the_prompt() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("coder.md"), "   \n\n").expect("write");
    assert_eq!(load(Some(dir.path()), "coder.md", CODER), CODER);
}

/// The rule the whole module exists for: editing the file must change what a run sees,
/// without a rebuild. The baked copy is a snapshot of the same text, so if the on-disk read
/// ever silently stopped working this would be the test that noticed.
#[test]
fn editing_the_file_changes_the_prompt_without_touching_the_binary() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("session-critic.md");
    std::fs::write(&path, "first wording").expect("write");
    assert_eq!(
        load(Some(dir.path()), "session-critic.md", SESSION_CRITIC),
        "first wording"
    );

    std::fs::write(&path, "second wording").expect("rewrite");
    assert_eq!(
        load(Some(dir.path()), "session-critic.md", SESSION_CRITIC),
        "second wording",
        "a prompt change must take effect on the next run, not the next build"
    );
}
