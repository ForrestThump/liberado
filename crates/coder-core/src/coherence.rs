//! # Contract coherence (S7-c)
//!
//! [`validate_draft`](crate::validate_draft) checks each verifier is well-formed **in isolation**:
//! a command has a program, a paths check has paths. Nothing checked the draft **against itself** —
//! and freeze then stamps a `content_hash` and makes it *binding*. So an incoherent contract is not
//! a soft error the worker muddles through: it is a durably authoritative instruction to do
//! something impossible, and the worker will faithfully execute it into the ground.
//!
//! This is the same shape as F1 (an artifact with authority that nothing validates), and it showed
//! up the same way — in a live run. Three runs of one coding session produced **four** distinct
//! incoherences:
//!
//! | What the model wrote | Why it broke | Caught here? |
//! |---|---|---|
//! | `out_of_scope: "no clippy or fmt"` while `cargo-clippy` and `cargo-fmt` sat in the verifier list | `verify_profile = "rust-strict"` silently re-added them; the prose and the list disagreed and the human reads the prose | **Yes** — [`Severity::Contradiction`] |
//! | Two `cargo test --all` verifiers, one from the model and one from the profile | Duplicate gate, and a sign the model did not know what the profile had added | **Yes** — [`Severity::Contradiction`] |
//! | `paths_exist: target/release/todo` (crate is `todo-cli`, and Windows needs `.exe`) | A gate that can never pass, dressed as diligence | **Warning** |
//! | `out_of_scope: "Modifying TOKEN.md"` + a verifier that only `TOKEN.md` could satisfy | The build could not pass without touching a file the contract forbade touching | **Warning** (see below) |
//!
//! ## What this deliberately cannot do
//!
//! The `TOKEN.md` case — the one that actually killed a live run — is **not statically decidable**.
//! The verifier was `Command { program: "powershell", args: ["-File", "check-token.ps1"] }`, and a
//! command verifier is an opaque black box: nothing in the contract says it reads `TOKEN.md`. No
//! amount of cross-referencing the *declared* fields would have found it.
//!
//! Pretending otherwise would be the exact trap this project keeps falling into — a check that
//! looks like it covers a class of bug and does not. So the mitigation is honest and indirect: an
//! `out_of_scope` line that names a **file** is *flagged as a warning*, because scope is meant to
//! describe **what not to build**, not which files are untouchable (that is what `PathPolicy` is
//! for). A human reading "this forbids touching a file; if any gate needs that file, the build can
//! never pass" has a real chance of catching it. A regex does not.

use crate::intake::{GoalContractDraft, profile_verifiers};
use crate::verify::VerifierSpec;

/// How bad a coherence finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// The contract contradicts itself. Freezing it would bind the worker to something impossible,
    /// so freeze **refuses** and the finding goes back to intake as a revision request.
    Contradiction,
    /// Suspicious, but a human might mean it. Surfaced in the freeze prompt rather than blocking —
    /// judgment belongs to the person, as long as they are actually shown the thing to judge.
    Warning,
}

#[derive(Debug, Clone)]
pub struct ContractFinding {
    pub severity: Severity,
    pub message: String,
}

impl ContractFinding {
    fn contradiction(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Contradiction,
            message: message.into(),
        }
    }
    fn warning(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
        }
    }
}

/// Check a draft contract against **itself**. Run after `verify_profile` expansion, so the verifier
/// list being checked is the one the worker will actually be judged against.
pub fn contract_conflicts(draft: &GoalContractDraft) -> Vec<ContractFinding> {
    let mut findings = Vec::new();
    findings.extend(scope_contradicts_a_verifier(draft));
    findings.extend(duplicate_verifiers(draft));
    findings.extend(scope_names_a_file(draft));
    findings.extend(unbuildable_path_verifiers(draft));
    findings
}

/// Every findings that must block a freeze.
pub fn contradictions(draft: &GoalContractDraft) -> Vec<ContractFinding> {
    contract_conflicts(draft)
        .into_iter()
        .filter(|f| f.severity == Severity::Contradiction)
        .collect()
}

/// Ordinary build vocabulary. A verifier token drawn from this list identifies **nothing** — these
/// are the words people use to write perfectly coherent scope lines.
///
/// This list is not fastidiousness, it is a scar. Without it the check fired on the sentence
/// *"Any other commands or verifiers beyond the three specified"* — because the default id of a
/// `Command` verifier is, literally, `"command"`. The model was told its coherent contract
/// contradicted itself, redrafted, wrote another sensible line containing "test" or "build", tripped
/// it again, and burned its whole budget. A linter that cries wolf is worse than no linter, and this
/// one did it live, in the first real run after it shipped.
const GENERIC: &[&str] = &[
    "command",
    "commands",
    "cargo",
    "build",
    "builds",
    "test",
    "tests",
    "check",
    "checks",
    "run",
    "runs",
    "release",
    "all",
    "file",
    "files",
    "path",
    "paths",
    "powershell",
    "bash",
    "sh",
    "node",
    "npm",
    "python",
    "script",
    "scripts",
    "exec",
    "verifier",
    "verifiers",
];

/// The **distinctive** words by which a verifier can be recognised in prose — `clippy`, `fmt`, a
/// hand-written id. Generic build vocabulary is excluded: it names every verifier and therefore
/// none.
fn verifier_tokens(v: &VerifierSpec) -> Vec<String> {
    let mut tokens = vec![v.id().to_lowercase()];
    if let VerifierSpec::Command { program, args, .. } = v {
        tokens.push(program.to_lowercase());
        tokens.extend(
            args.iter()
                // Skip flags and paths; `--all-targets` and `-File` are not what anyone means.
                .filter(|a| !a.starts_with('-') && !a.contains('/') && !a.contains('\\'))
                .map(|a| a.to_lowercase()),
        );
    }
    tokens
        .into_iter()
        // Two-letter tokens match everything; they are noise, not signal.
        .filter(|t| t.len() > 2 && !GENERIC.contains(&t.as_str()))
        .collect()
}

/// The one that bit us twice: the model declares a gate out of scope while the gate is in the list.
///
/// `out_of_scope` is inherently a *prohibition*, so naming a live verifier in it is not a nuance —
/// it is the contract disagreeing with itself about what the work will be judged on. It happened
/// because `verify_profile` re-added verifiers behind the model's back, so its prose was sincere
/// and its list was authoritative, and the human reads the prose.
fn scope_contradicts_a_verifier(draft: &GoalContractDraft) -> Vec<ContractFinding> {
    let mut findings = Vec::new();
    for line in &draft.out_of_scope {
        let lower = line.to_lowercase();
        for v in &draft.verifiers {
            for token in verifier_tokens(v) {
                if lower.contains(&token) {
                    findings.push(ContractFinding::contradiction(format!(
                        "out of scope says \"{}\", but `{}` is one of the verifiers this will be \
                         judged against. A gate cannot be both out of scope and binding. (If the \
                         verifier came from `verify_profile`, clear the profile — editing the \
                         verifier list will not remove it.)",
                        line.trim(),
                        v.id()
                    )));
                    break;
                }
            }
        }
    }
    findings
}

/// Two gates that run the same command. Harmless on its own — but it is the fingerprint of a model
/// that does not know what its own `verify_profile` added, which is never *only* harmless.
fn duplicate_verifiers(draft: &GoalContractDraft) -> Vec<ContractFinding> {
    let mut findings = Vec::new();
    let key = |v: &VerifierSpec| -> Option<String> {
        match v {
            VerifierSpec::Command { program, args, .. } => {
                Some(format!("{} {}", program.to_lowercase(), args.join(" ")))
            }
            _ => None,
        }
    };
    let mut seen: Vec<(String, String)> = Vec::new();
    for v in &draft.verifiers {
        let Some(k) = key(v) else { continue };
        if let Some((_, first_id)) = seen.iter().find(|(existing, _)| *existing == k) {
            findings.push(ContractFinding::contradiction(format!(
                "verifiers `{}` and `{}` run the same command (`{}`). The duplicate is usually a \
                 `verify_profile` re-adding a gate the model had already written by hand — which \
                 means the model does not know what it is actually being judged on.",
                first_id,
                v.id(),
                k.trim()
            )));
        } else {
            seen.push((k, v.id().to_string()));
        }
    }
    findings
}

/// `out_of_scope` naming a **file** is a category error, and it is how a live run died: the
/// contract required a gate that only `TOKEN.md` could satisfy, and forbade writing `TOKEN.md`.
///
/// A warning, not a contradiction, because we cannot prove the conflict — a `Command` verifier is
/// opaque, and nothing declares that it reads that file. What we *can* do is put the sentence in
/// front of the human with the consequence spelled out.
fn scope_names_a_file(draft: &GoalContractDraft) -> Vec<ContractFinding> {
    draft
        .out_of_scope
        .iter()
        .filter_map(|line| {
            let file = line.split_whitespace().find(|w| looks_like_a_path(w))?;
            Some(ContractFinding::warning(format!(
                "out of scope names a file (`{}`): \"{}\". Scope should say what NOT to build, not \
                 which files are untouchable — that is what the path policy is for. If any \
                 verifier needs that file, the build can never pass, and nothing here can detect \
                 that (a command verifier is opaque). This is exactly how a live run died.",
                file.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '/'),
                line.trim()
            )))
        })
        .collect()
}

/// A heuristic for spotting filename-like tokens in out-of-scope prose: the word contains a `/` or
/// a `.` (but does not end with `.`), is longer than 3 chars, starts with an alphanumeric, and has
/// a plausible 2-5 character ASCII extension after the last dot.
///
/// Extracted from the closure inside [`scope_names_a_file`] so boundary conditions (length exactly
/// 3, extension edge cases) are directly unit-testable.
fn looks_like_a_path(word: &str) -> bool {
    let w = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '/' && c != '_');
    (w.contains('/') || w.contains('.'))
        && !w.ends_with('.')
        && w.len() > 3
        && w.chars().next().is_some_and(|c| c.is_alphanumeric())
        // Not a sentence ("todos.", "e.g.") — a real filename has a plausible extension.
        && w.rsplit('.').next().is_some_and(|ext| {
            (2..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric())
        })
}

/// A `paths_exist` gate on a build artifact (`target/`, `node_modules/`, `.git/`) is fragile at
/// best and unsatisfiable at worst: the path depends on the crate name, the profile, and the
/// platform's executable suffix. A live draft asked for `target/release/todo` when the crate was
/// `todo-cli` — on Windows, where it would have been `todo-cli.exe`. It could never have passed.
fn unbuildable_path_verifiers(draft: &GoalContractDraft) -> Vec<ContractFinding> {
    const FRAGILE: &[&str] = &["target/", "node_modules/", ".git/", "dist/", "build/"];
    let mut findings = Vec::new();
    for v in &draft.verifiers {
        if let VerifierSpec::PathsExist { id, paths } = v {
            for p in paths {
                let norm = p.replace('\\', "/");
                if FRAGILE.iter().any(|f| norm.starts_with(f)) {
                    findings.push(ContractFinding::warning(format!(
                        "verifier `{id}` requires `{p}` to exist — a build artifact. Its real path \
                         depends on the crate name, the build profile and the platform's \
                         executable suffix, so this gate is easy to write in a form that can never \
                         pass. Prefer a command verifier (`cargo build --release`) that checks the \
                         build *works* rather than guessing where it lands."
                    )));
                }
            }
        }
    }
    findings
}

/// Which of `draft`'s verifiers were injected by `verify_profile` rather than written for this
/// goal. The human is shown this: the model's prose said it had dropped clippy while clippy sat in
/// the list, and there was no way to tell from the prompt where it came from.
pub fn profile_injected_ids(draft: &GoalContractDraft) -> Vec<String> {
    let Some(name) = &draft.verify_profile else {
        return Vec::new();
    };
    let injected: Vec<String> = profile_verifiers(name)
        .iter()
        .map(|v| v.id().to_string())
        .collect();
    draft
        .verifiers
        .iter()
        .map(|v| v.id().to_string())
        .filter(|id| injected.contains(id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::VerifierSpec;

    fn cmd(id: &str, program: &str, args: &[&str]) -> VerifierSpec {
        VerifierSpec::Command {
            id: id.into(),
            program: program.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            env: Default::default(),
            timeout_secs: None,
            output_max_bytes: None,
            network: false,
        }
    }

    fn draft(verifiers: Vec<VerifierSpec>, out_of_scope: Vec<&str>) -> GoalContractDraft {
        GoalContractDraft {
            description: "build a todo cli".into(),
            success_criteria: vec!["it works".into()],
            verifiers,
            out_of_scope: out_of_scope.into_iter().map(String::from).collect(),
            assumed_defaults: vec![],
            domain_hint: None,
            verify_profile: None,
        }
    }

    #[test]
    fn declaring_a_live_verifier_out_of_scope_is_a_contradiction() {
        // The real one, twice over: `verify_profile = "rust-strict"` re-added clippy behind the
        // model's back, so its prose sincerely said "no clippy" while clippy was binding.
        let d = draft(
            vec![cmd("cargo-clippy", "cargo", &["clippy", "--all-targets"])],
            vec!["No clippy or fmt checks."],
        );
        let found = contradictions(&d);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(found[0].message.contains("cargo-clippy"));
        assert!(
            found[0].message.contains("verify_profile"),
            "must tell them WHY editing the verifier list will not help: {}",
            found[0].message
        );
    }

    #[test]
    fn two_verifiers_running_the_same_command_is_a_contradiction() {
        let d = draft(
            vec![
                cmd("tests", "cargo", &["test", "--all"]),
                cmd("cargo-test", "cargo", &["test", "--all"]),
            ],
            vec![],
        );
        assert_eq!(contradictions(&d).len(), 1);
    }

    #[test]
    fn a_coherent_contract_has_nothing_to_say() {
        // The contract that actually built the thing, live. It must not trip a single check —
        // a linter that cries wolf on a working contract is worse than none.
        let d = draft(
            vec![
                cmd("build", "cargo", &["build", "--release"]),
                cmd("tests", "cargo", &["test", "--all"]),
                cmd(
                    "release-gate",
                    "powershell",
                    &["-NoProfile", "-File", "C:/x/check-token.ps1"],
                ),
            ],
            vec!["Additional features like delete, update, or mark done"],
        );
        let found = contract_conflicts(&d);
        assert!(
            found.is_empty(),
            "false positives on a good contract: {found:#?}"
        );
    }

    #[test]
    fn scope_that_forbids_touching_a_file_is_flagged() {
        // Not provable — a command verifier is opaque — so it is a WARNING with the consequence
        // spelled out, not a block. This is the sentence that killed a live run.
        let d = draft(
            vec![cmd("gate", "powershell", &["-File", "check.ps1"])],
            vec!["Modifying TOKEN.md or guessing the release token"],
        );
        let found = contract_conflicts(&d);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].severity, Severity::Warning);
        assert!(
            found[0].message.contains("TOKEN.md"),
            "{}",
            found[0].message
        );
        assert!(
            contradictions(&d).is_empty(),
            "a warning must not block the freeze — we cannot prove it"
        );
    }

    #[test]
    fn a_paths_exist_gate_on_a_build_artifact_warns() {
        let d = GoalContractDraft {
            verifiers: vec![VerifierSpec::PathsExist {
                id: "binary".into(),
                paths: vec!["target/release/todo".into()],
            }],
            ..draft(vec![], vec![])
        };
        let found = contract_conflicts(&d);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].severity, Severity::Warning);
        assert!(found[0].message.contains("target/release/todo"));
    }

    #[test]
    fn profile_injected_verifiers_are_identifiable() {
        // So the freeze prompt can say "3 from you, 3 added by rust-strict" — without which the
        // model's prose and its own verifier list can disagree and nobody can tell.
        let mut d = draft(vec![cmd("build", "cargo", &["build"])], vec![]);
        d.verify_profile = Some("rust-strict".into());
        crate::expand_verify_profile_into(&mut d);

        let injected = profile_injected_ids(&d);
        assert!(
            injected.contains(&"cargo-clippy".to_string()),
            "{injected:?}"
        );
        assert!(
            !injected.contains(&"build".to_string()),
            "the human's own verifier is not profile-injected: {injected:?}"
        );
    }

    #[test]
    fn ordinary_scope_prose_is_not_a_contradiction() {
        // THE REGRESSION. Every line below is a perfectly coherent thing to write, and every one of
        // them tripped the check when it shipped -- because the default id of a Command verifier is
        // literally "command", and "build"/"test"/"release" are English. Live, the model was told
        // its good contract contradicted itself, redrafted, tripped it again, and the session died
        // WITHOUT EVER ASKING THE HUMAN ANYTHING.
        //
        // My original "no false positives" test used scope lines with no build vocabulary in them,
        // which is precisely the case that could never have failed. A no-false-positive test that
        // avoids the words the domain is made of is not testing anything.
        let d = draft(
            vec![
                cmd("command", "cargo", &["build", "--release"]),
                cmd("command", "cargo", &["test", "--all"]),
                cmd("release-gate", "powershell", &["-File", "check.ps1"]),
            ],
            vec![
                "Any other commands or verifiers beyond the three specified",
                "No deployment or packaging beyond the release binary",
                "No additional test frameworks",
                "Nothing outside the build itself",
                "No changes to files under path/to/other",
            ],
        );
        let found = contradictions(&d);
        assert!(
            found.is_empty(),
            "a linter that cries wolf is worse than no linter: {found:#?}"
        );
    }

    #[test]
    fn a_distinctive_gate_named_out_of_scope_is_still_caught() {
        // ...and the real one must still fire. `clippy` and `fmt` are not English; they name a
        // specific gate, and declaring one out of scope while it is binding is a genuine conflict.
        let d = draft(
            vec![
                cmd("cargo-clippy", "cargo", &["clippy", "--all-targets"]),
                cmd("command", "cargo", &["build", "--release"]),
            ],
            vec![
                "No clippy or fmt checks.",
                "No deployment beyond the release build",
            ],
        );
        let found = contradictions(&d);
        assert_eq!(
            found.len(),
            1,
            "exactly the clippy conflict, and nothing from the ordinary line: {found:#?}"
        );
        assert!(found[0].message.contains("cargo-clippy"));
    }

    #[test]
    fn verifier_tokens_skips_flags_and_paths() {
        let v = VerifierSpec::Command {
            id: "check".into(),
            program: "mypgm".into(),
            args: vec![
                "-v".into(),
                "/etc/passwd".into(),
                "lint".into(),
                "C:\\foo".into(),
            ],
            env: Default::default(),
            timeout_secs: None,
            output_max_bytes: None,
            network: false,
        };
        let tokens = verifier_tokens(&v);
        assert!(tokens.contains(&"mypgm".into()));
        assert!(tokens.contains(&"lint".into()));
        assert!(!tokens.iter().any(|t| t.starts_with('-')));
        assert!(!tokens.iter().any(|t| t.contains('/')));
        assert!(!tokens.iter().any(|t| t.contains('\\')));
    }

    #[test]
    fn verifier_tokens_removes_short_and_generic_tokens() {
        let v = VerifierSpec::Command {
            id: "custom_check".into(),
            program: "npm".into(),
            args: vec!["run".into(), "test".into(), "ab".into()],
            env: Default::default(),
            timeout_secs: None,
            output_max_bytes: None,
            network: false,
        };
        let tokens = verifier_tokens(&v);
        // "run", "npm", "test" are all GENERIC; "ab" is <= 2 chars.
        // Only "custom_check" (12 chars, not generic) survives.
        assert!(tokens.contains(&"custom_check".into()));
        assert!(!tokens.contains(&"run".into()));
        assert!(!tokens.contains(&"ab".into()));
    }

    #[test]
    fn looks_like_a_path_accepts_valid_filenames() {
        assert!(looks_like_a_path("src/main.rs"));
        assert!(looks_like_a_path("notes.md"));
    }

    #[test]
    fn looks_like_a_path_rejects_short_names() {
        assert!(!looks_like_a_path("a.b"), "3 chars — not > 3");
        assert!(!looks_like_a_path("x.y"), "3 chars — not > 3");
    }

    #[test]
    fn looks_like_a_path_rejects_end_of_sentence() {
        assert!(!looks_like_a_path("todos."), "trailing dot is not a path");
        assert!(!looks_like_a_path("e.g."), "trailing dot, generic ext/word");
    }

    #[test]
    fn looks_like_a_path_accepts_exactly_4_chars() {
        assert!(looks_like_a_path("a.py"), "4 chars — len > 3");
    }

    #[test]
    fn looks_like_a_path_rejects_3_chars_with_extension() {
        assert!(!looks_like_a_path("a.b"), "3 chars — not > 3");
    }

    #[test]
    fn looks_like_a_path_rejects_extension_with_non_alnum() {
        assert!(!looks_like_a_path("src/file.c++"));
    }

    #[test]
    fn looks_like_a_path_strips_leading_punctuation() {
        // Backtick-prefixed in markdown: `src/main.rs` should match.
        assert!(looks_like_a_path("`src/main.rs`"), "should strip backticks");
    }

    #[test]
    fn looks_like_a_path_strips_trailing_paren() {
        // "...in src/main.rs (" should match.
        assert!(looks_like_a_path("src/main.rs)"), "should strip trailing paren");
    }
}
