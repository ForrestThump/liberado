//! Criteria intake DTOs — thinking model turns a human writeup into a draft contract.
//!
//! See `docs/spec/architecture/verifiers.md` §3. Freeze is a product/UI step; these types are the
//! structured output surface for `complete_json`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

use crate::verify::VerifierSpec;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum IntakeOutcome {
    NeedsClarification {
        questions: Vec<IntakeQuestion>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        partial_draft: Option<GoalContractDraft>,
    },
    ReadyForFreeze {
        draft: GoalContractDraft,
        #[serde(default)]
        rationale: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntakeQuestion {
    /// Models sometimes emit `prompt` as a string array (dogfood finding #2 — DeepSeek under
    /// `json_object` fallback). Accept string or sequence-of-strings joined with newlines.
    #[serde(deserialize_with = "deserialize_string_flexible")]
    pub id: String,
    #[serde(deserialize_with = "deserialize_string_flexible")]
    pub prompt: String,
    /// Models sometimes return a single string; accept string or array.
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub options: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_flexible")]
    pub affects: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalContractDraft {
    pub description: String,
    /// Models often emit one prose criterion as a string; accept string or array for live intake.
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub success_criteria: Vec<String>,
    /// Skip unknown/malformed entries (live models sometimes stuff `verify_profile` into this list).
    #[serde(default, deserialize_with = "deserialize_verifier_list")]
    pub verifiers: Vec<VerifierSpec>,
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub out_of_scope: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub assumed_defaults: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_profile: Option<String>,
}

/// Parse verifier list, dropping entries that are not valid `VerifierSpec` (do not fail intake).
fn deserialize_verifier_list<'de, D>(deserializer: D) -> Result<Vec<VerifierSpec>, D::Error>
where
    D: Deserializer<'de>,
{
    let values: Vec<serde_json::Value> = Deserialize::deserialize(deserializer)?;
    let mut out = Vec::new();
    for v in values {
        // Misplaced profile hint — not a verifier type.
        if v.get("type").and_then(|t| t.as_str()) == Some("verify_profile") {
            continue;
        }
        match serde_json::from_value::<VerifierSpec>(v) {
            Ok(spec) => out.push(spec),
            Err(_) => continue,
        }
    }
    Ok(out)
}

/// Accept a plain string, or a sequence of strings (joined with newlines), or a scalar coerced
/// via `Display`. Live models under unconstrained `json_object` often put arrays where a string
/// field is required (dogfood finding #2).
pub(crate) fn deserialize_string_flexible<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::{self, SeqAccess, Visitor};
    use std::fmt;

    struct FlexString;

    impl<'de> Visitor<'de> for FlexString {
        type Value = String;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a string, sequence of strings, or scalar")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            Ok(v.to_string())
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
            Ok(v)
        }

        fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
            Ok(v.to_string())
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
            Ok(v.to_string())
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
            Ok(v.to_string())
        }

        fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
            Ok(v.to_string())
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(String::new())
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(String::new())
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut parts = Vec::new();
            while let Some(item) = seq.next_element::<serde_json::Value>()? {
                match item {
                    serde_json::Value::String(s) => parts.push(s),
                    serde_json::Value::Null => {}
                    other => parts.push(other.to_string()),
                }
            }
            Ok(parts.join("\n"))
        }
    }

    deserializer.deserialize_any(FlexString)
}

/// Accept `["a","b"]` or `"a"` (wrapped as a one-element vec). Live models frequently mess this up.
/// Pub(crate) so `VerifierSpec` fields can reuse it.
pub(crate) fn deserialize_string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::{self, SeqAccess, Visitor};
    use std::fmt;

    struct StringOrVec;

    impl<'de> Visitor<'de> for StringOrVec {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a string, sequence of strings, or empty")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            if v.trim().is_empty() {
                Ok(Vec::new())
            } else {
                Ok(vec![v.to_string()])
            }
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
            self.visit_str(&v)
        }

        fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
            Ok(vec![v.to_string()])
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
            Ok(vec![v.to_string()])
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
            Ok(vec![v.to_string()])
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            // Accept strings, numbers, or nested values coerced via JSON.
            while let Some(item) = seq.next_element::<serde_json::Value>()? {
                match item {
                    serde_json::Value::String(s) => out.push(s),
                    serde_json::Value::Number(n) => out.push(n.to_string()),
                    serde_json::Value::Bool(b) => out.push(b.to_string()),
                    serde_json::Value::Null => {}
                    other => {
                        // Object/array in list: stringify compactly rather than fail intake.
                        if let Ok(s) = serde_json::to_string(&other) {
                            out.push(s);
                        }
                    }
                }
            }
            Ok(out)
        }

        fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            // Live models sometimes emit objects where a list is expected — ignore keys, keep values.
            let mut out = Vec::new();
            while let Some((_, v)) = map.next_entry::<String, serde_json::Value>()? {
                match v {
                    serde_json::Value::String(s) => out.push(s),
                    serde_json::Value::Number(n) => out.push(n.to_string()),
                    serde_json::Value::Bool(b) => out.push(b.to_string()),
                    serde_json::Value::Null => {}
                    other => {
                        if let Ok(s) = serde_json::to_string(&other) {
                            out.push(s);
                        }
                    }
                }
            }
            Ok(out)
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
    }

    deserializer.deserialize_any(StringOrVec)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreezeAuthority {
    Human,
    PolicyAuto { rule_id: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalContract {
    pub id: String,
    pub draft: GoalContractDraft,
    pub frozen_at: DateTime<Utc>,
    pub frozen_by: FreezeAuthority,
    pub content_hash: String,
}

impl GoalContract {
    /// Freeze a draft after human accept (or policy auto). Computes a stable content hash.
    /// Expands `verify_profile` into concrete verifiers before hashing.
    ///
    /// Refuses a contract that **contradicts itself** (S7-c). Freezing is what makes the gates
    /// authoritative — the worker cannot argue with them — so binding it to something impossible is
    /// not a soft error it muddles through: it obeys, faithfully, into the ground. A live run did
    /// exactly that, building against a contract that demanded a gate only `TOKEN.md` could satisfy
    /// while forbidding it to write `TOKEN.md`. The worker was right and the contract was wrong.
    ///
    /// Only *contradictions* block. Warnings are the human's judgement to make, and are shown to
    /// them at the freeze prompt rather than decided here.
    pub fn freeze(
        id: impl Into<String>,
        mut draft: GoalContractDraft,
        frozen_by: FreezeAuthority,
    ) -> Result<Self, String> {
        sanitize_draft(&mut draft);
        expand_verify_profile_into(&mut draft);
        validate_draft(&draft)?;
        // After expansion: the list checked here is the one the worker is actually judged against,
        // which is the whole point — `verify_profile` adds gates the model does not know about.
        let contradictions = crate::coherence::contradictions(&draft);
        if !contradictions.is_empty() {
            return Err(format!(
                "the contract contradicts itself, so it cannot be frozen:\n{}",
                contradictions
                    .iter()
                    .map(|c| format!("  - {}", c.message))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        let frozen_at = Utc::now();
        let content_hash = hash_draft(&draft);
        Ok(Self {
            id: id.into(),
            draft,
            frozen_at,
            frozen_by,
            content_hash,
        })
    }

    /// Re-derive the hash and check it still matches the draft this contract carries.
    ///
    /// This is what makes [`content_hash`](Self::content_hash) mean something. A contract's whole
    /// purpose is that the gates were agreed by a **human** at freeze time — so anything that
    /// receives a contract second-hand (deserialized from a transcript, handed across a process,
    /// passed to the worker that will be graded by it) must be able to prove the gates it holds are
    /// the gates that were accepted, not ones edited afterwards. Call this before *acting* on a
    /// contract you did not freeze yourself.
    pub fn verify_integrity(&self) -> Result<(), String> {
        let actual = hash_draft(&self.draft);
        if actual == self.content_hash {
            Ok(())
        } else {
            Err(format!(
                "contract '{}' has been modified since it was frozen \
                 (expected {}, got {})",
                self.id, self.content_hash, actual
            ))
        }
    }

    /// Stamp a frozen contract onto a coding run request (description, prose criteria, verifiers).
    pub fn apply_to_request(&self, request: &mut crate::CoderRunRequest) {
        request.task.description = self.draft.description.clone();
        request.task.success_criteria = self.draft.success_criteria.clone();
        request.config.verifiers = self.draft.verifiers.clone();
        // Prefer explicit pipeline over a single legacy command when the contract has checks.
        if !request.config.verifiers.is_empty() {
            request.config.validation_command = None;
        }
    }
}

/// Merge named profiles into `draft.verifiers` (by id; existing ids win).
pub fn expand_verify_profile_into(draft: &mut GoalContractDraft) {
    let Some(name) = draft.verify_profile.clone() else {
        return;
    };
    let profile = profile_verifiers(&name);
    if profile.is_empty() {
        return;
    }
    let existing: std::collections::HashSet<String> =
        draft.verifiers.iter().map(|v| v.id().to_string()).collect();
    for spec in profile {
        if !existing.contains(spec.id()) {
            draft.verifiers.push(spec);
        }
    }
}

/// Built-in stacks — data, not kernel language locks. Projects can still hand-author verifiers.
pub fn profile_verifiers(name: &str) -> Vec<VerifierSpec> {
    match name.trim() {
        "rust-check" => vec![VerifierSpec::Command {
            id: "cargo-check".into(),
            program: "cargo".into(),
            args: vec!["check".into()],
            env: Default::default(),
            timeout_secs: Some(300),
            output_max_bytes: Some(64 * 1024),
            network: false,
        }],
        "rust-strict" => vec![
            VerifierSpec::Command {
                id: "cargo-test".into(),
                program: "cargo".into(),
                args: vec!["test".into(), "--all".into()],
                env: Default::default(),
                timeout_secs: Some(600),
                output_max_bytes: Some(64 * 1024),
                network: false,
            },
            VerifierSpec::Command {
                id: "cargo-clippy".into(),
                program: "cargo".into(),
                args: vec![
                    "clippy".into(),
                    "--all-targets".into(),
                    "--".into(),
                    "-D".into(),
                    "warnings".into(),
                ],
                env: Default::default(),
                timeout_secs: Some(600),
                output_max_bytes: Some(64 * 1024),
                network: false,
            },
            VerifierSpec::Command {
                id: "cargo-fmt".into(),
                program: "cargo".into(),
                args: vec!["fmt".into(), "--".into(), "--check".into()],
                env: Default::default(),
                timeout_secs: Some(120),
                output_max_bytes: Some(32 * 1024),
                network: false,
            },
        ],
        "node-test" => vec![VerifierSpec::Command {
            id: "npm-test".into(),
            program: "npm".into(),
            args: vec!["test".into()],
            env: Default::default(),
            timeout_secs: Some(300),
            output_max_bytes: Some(64 * 1024),
            network: false,
        }],
        _ => Vec::new(),
    }
}

/// Drop incomplete verifiers from live-model JSON (missing program/paths/etc.) instead of failing
/// the whole intake. Keeps git_nonempty_diff and well-formed checks.
pub fn sanitize_draft(draft: &mut GoalContractDraft) {
    draft.verifiers.retain(|v| match v {
        VerifierSpec::Command { program, .. } => !program.trim().is_empty(),
        VerifierSpec::PathsExist { paths, .. } | VerifierSpec::PathsAbsent { paths, .. } => {
            !paths.is_empty()
        }
        VerifierSpec::ContentContains {
            path, must_include, ..
        } => !path.trim().is_empty() && !must_include.is_empty(),
        VerifierSpec::GitNonemptyDiff { .. } => true,
    });
    // Ensure description is trimmed; empty still fails validate.
    draft.description = draft.description.trim().to_string();
}

/// Reject obviously unsafe or empty drafts before freeze.
pub fn validate_draft(draft: &GoalContractDraft) -> Result<(), String> {
    if draft.description.trim().is_empty() {
        return Err("draft description must not be empty".into());
    }
    for v in &draft.verifiers {
        match v {
            VerifierSpec::Command {
                program,
                network,
                id,
                ..
            } => {
                if program.trim().is_empty() {
                    return Err(format!("verifier {id}: command program is empty"));
                }
                // Soft warn path: network true is allowed but must be explicit (already is).
                let _ = network;
            }
            VerifierSpec::PathsExist { paths, id, .. }
            | VerifierSpec::PathsAbsent { paths, id, .. } => {
                if paths.is_empty() {
                    return Err(format!("verifier {id}: paths list is empty"));
                }
            }
            VerifierSpec::ContentContains {
                path,
                must_include,
                id,
                ..
            } => {
                if path.trim().is_empty() || must_include.is_empty() {
                    return Err(format!(
                        "verifier {id}: content_contains needs path and must_include"
                    ));
                }
            }
            VerifierSpec::GitNonemptyDiff { .. } => {}
        }
    }
    Ok(())
}

/// Content digest of a draft — the identity of the *agreed* gates.
///
/// A real SHA-256, per `verifiers.md` §7 (`"content_hash": "sha256:…"`). This used to be
/// `DefaultHasher` behind a `sha256-lite:` label, which was wrong in three ways that all matter for
/// a field whose stated job is integrity: it is not collision-resistant, it is trivially
/// forgeable, and `DefaultHasher`'s output is explicitly **not stable across Rust releases** — so a
/// contract frozen by one build could fail to verify against the next one, for no reason at all.
///
/// The draft is serialized to JSON first; serde emits struct fields in declaration order, so the
/// encoding is deterministic for a given draft.
fn hash_draft(draft: &GoalContractDraft) -> String {
    use sha2::{Digest, Sha256};
    let json = serde_json::to_string(draft).unwrap_or_default();
    let digest = Sha256::digest(json.as_bytes());
    format!("sha256:{digest:x}")
}

/// JSON Schema fragment for intake structured output (OpenAI-compatible json_schema mode).
pub fn intake_outcome_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["status"],
        "properties": {
            "status": {
                "type": "string",
                "enum": ["needs_clarification", "ready_for_freeze"]
            },
            "questions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["id", "prompt"],
                    "properties": {
                        "id": { "type": "string" },
                        "prompt": { "type": "string" },
                        "options": { "type": "array", "items": { "type": "string" } },
                        "affects": { "type": "string" }
                    }
                }
            },
            "partial_draft": { "type": "object" },
            "draft": { "type": "object" },
            "rationale": { "type": "string" }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifiers_skip_unknown_type_entries() {
        let raw = r#"{
            "status": "ready_for_freeze",
            "draft": {
                "description": "todo",
                "success_criteria": ["works"],
                "verifiers": [
                    {"type": "verify_profile", "name": "rust-check"},
                    {"type": "paths_exist", "paths": ["src/main.rs"]},
                    {"type": "not_a_real_kind", "id": "x"}
                ]
            }
        }"#;
        let outcome: IntakeOutcome = serde_json::from_str(raw).unwrap();
        match outcome {
            IntakeOutcome::ReadyForFreeze { draft, .. } => {
                assert_eq!(draft.verifiers.len(), 1);
                assert_eq!(draft.verifiers[0].id(), "paths_exist");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn success_criteria_accepts_single_string() {
        let raw = r#"{
            "status": "ready_for_freeze",
            "draft": {
                "description": "todo cli",
                "success_criteria": "add and list work",
                "verifiers": [],
                "out_of_scope": "no network",
                "assumed_defaults": ["Rust"]
            },
            "rationale": "ok"
        }"#;
        let outcome: IntakeOutcome = serde_json::from_str(raw).unwrap();
        match outcome {
            IntakeOutcome::ReadyForFreeze { draft, .. } => {
                assert_eq!(
                    draft.success_criteria,
                    vec!["add and list work".to_string()]
                );
                assert_eq!(draft.out_of_scope, vec!["no network".to_string()]);
                assert_eq!(draft.assumed_defaults, vec!["Rust".to_string()]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn freeze_rejects_empty_description() {
        let draft = GoalContractDraft {
            description: "  ".into(),
            success_criteria: vec![],
            verifiers: vec![],
            out_of_scope: vec![],
            assumed_defaults: vec![],
            domain_hint: None,
            verify_profile: None,
        };
        assert!(GoalContract::freeze("g1", draft, FreezeAuthority::Human).is_err());
    }

    #[test]
    fn freeze_stamps_hash() {
        let draft = GoalContractDraft {
            description: "Build a todo CLI".into(),
            success_criteria: vec!["add and list work".into()],
            verifiers: vec![VerifierSpec::PathsExist {
                id: "paths".into(),
                paths: vec!["src/main.rs".into()],
            }],
            out_of_scope: vec![],
            assumed_defaults: vec!["Rust".into()],
            domain_hint: Some("coding".into()),
            verify_profile: Some("rust-check".into()),
        };
        let c = GoalContract::freeze("g1", draft, FreezeAuthority::Human).unwrap();
        // A real SHA-256, per verifiers.md §7 — 64 hex chars behind a `sha256:` label.
        assert!(
            c.content_hash.starts_with("sha256:"),
            "got {}",
            c.content_hash
        );
        let digest = c.content_hash.strip_prefix("sha256:").unwrap();
        assert_eq!(digest.len(), 64);
        assert!(digest.chars().all(|ch| ch.is_ascii_hexdigit()));
        // Structural check + expanded rust-check profile (cargo-check).
        assert_eq!(c.draft.verifiers.len(), 2);
        assert!(c.draft.verifiers.iter().any(|v| v.id() == "cargo-check"));
        // A freshly frozen contract verifies against itself.
        c.verify_integrity().unwrap();
    }

    fn contract_for_tamper_tests() -> GoalContract {
        GoalContract::freeze(
            "g1",
            GoalContractDraft {
                description: "Build a todo CLI".into(),
                success_criteria: vec!["add and list work".into()],
                verifiers: vec![VerifierSpec::Command {
                    id: "cargo-test".into(),
                    program: "cargo".into(),
                    args: vec!["test".into()],
                    env: Default::default(),
                    timeout_secs: None,
                    output_max_bytes: None,
                    network: false,
                }],
                out_of_scope: vec![],
                assumed_defaults: vec![],
                domain_hint: None,
                verify_profile: None,
            },
            FreezeAuthority::Human,
        )
        .unwrap()
    }

    #[test]
    fn weakening_the_gates_after_freeze_is_detected() {
        // The attack the hash exists to catch: the contract said "cargo test must pass"; something
        // downstream quietly drops the gate so the work grades itself as done. The contract must no
        // longer verify — otherwise "frozen" means nothing.
        let mut c = contract_for_tamper_tests();
        c.verify_integrity().expect("pristine contract verifies");

        c.draft.verifiers.clear();
        let err = c
            .verify_integrity()
            .expect_err("dropped gates must be caught");
        assert!(err.contains("modified since it was frozen"), "got: {err}");
    }

    #[test]
    fn rewriting_the_goal_after_freeze_is_detected() {
        // The other half: the gates survive but the goal is swapped underneath them.
        let mut c = contract_for_tamper_tests();
        c.draft.description = "Build something else entirely".into();
        assert!(c.verify_integrity().is_err());
    }

    #[test]
    fn the_hash_is_stable_across_freezes_of_the_same_draft() {
        // `frozen_at` differs between these two, but the hash covers the *draft*, not the stamp —
        // so the same agreed content always has the same identity. (The old DefaultHasher was not
        // even stable across Rust releases, which would have made a stored contract fail to verify
        // after a toolchain bump, for no reason at all.)
        let a = contract_for_tamper_tests();
        let b = contract_for_tamper_tests();
        assert_eq!(a.content_hash, b.content_hash);
    }

    // ── StringOrVec visitor shapes ────────────────────────────────────────────
    // success_criteria, out_of_scope, and assumed_defaults use a custom serde
    // visitor that accepts many JSON shapes a model might emit.

    #[test]
    fn success_criteria_as_boolean() {
        let raw = r#"{"status":"ready_for_freeze","draft":{"description":"x","success_criteria":true,"verifiers":[],"out_of_scope":"","assumed_defaults":""}}"#;
        let outcome: IntakeOutcome = serde_json::from_str(raw).unwrap();
        match outcome {
            IntakeOutcome::ReadyForFreeze { draft, .. } => {
                assert_eq!(draft.success_criteria, vec!["true"]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn success_criteria_as_number() {
        let raw = r#"{"status":"ready_for_freeze","draft":{"description":"x","success_criteria":42,"verifiers":[],"out_of_scope":"","assumed_defaults":""}}"#;
        let outcome: IntakeOutcome = serde_json::from_str(raw).unwrap();
        match outcome {
            IntakeOutcome::ReadyForFreeze { draft, .. } => {
                assert_eq!(draft.success_criteria, vec!["42"]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn success_criteria_as_array_of_strings() {
        let raw = r#"{"status":"ready_for_freeze","draft":{"description":"x","success_criteria":["a","b"],"verifiers":[],"out_of_scope":"","assumed_defaults":""}}"#;
        let outcome: IntakeOutcome = serde_json::from_str(raw).unwrap();
        match outcome {
            IntakeOutcome::ReadyForFreeze { draft, .. } => {
                assert_eq!(draft.success_criteria, vec!["a", "b"]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn success_criteria_as_array_of_mixed_types() {
        let raw = r#"{"status":"ready_for_freeze","draft":{"description":"x","success_criteria":["a",42,true,null,{}],"verifiers":[],"out_of_scope":"","assumed_defaults":""}}"#;
        let outcome: IntakeOutcome = serde_json::from_str(raw).unwrap();
        match outcome {
            IntakeOutcome::ReadyForFreeze { draft, .. } => {
                assert_eq!(draft.success_criteria, vec!["a", "42", "true", "{}"]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn success_criteria_as_null() {
        let raw = r#"{"status":"ready_for_freeze","draft":{"description":"x","success_criteria":null,"verifiers":[],"out_of_scope":"","assumed_defaults":""}}"#;
        let outcome: IntakeOutcome = serde_json::from_str(raw).unwrap();
        match outcome {
            IntakeOutcome::ReadyForFreeze { draft, .. } => {
                assert!(draft.success_criteria.is_empty());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn success_criteria_as_empty_string() {
        let raw = r#"{"status":"ready_for_freeze","draft":{"description":"x","success_criteria":"   ","verifiers":[],"out_of_scope":"","assumed_defaults":""}}"#;
        let outcome: IntakeOutcome = serde_json::from_str(raw).unwrap();
        match outcome {
            IntakeOutcome::ReadyForFreeze { draft, .. } => {
                assert!(draft.success_criteria.is_empty());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn sanitize_draft_drops_incomplete_verifiers() {
        let mut draft = GoalContractDraft {
            description: "x".into(),
            success_criteria: vec![],
            verifiers: vec![
                VerifierSpec::Command {
                    id: "empty".into(),
                    program: String::new(),
                    args: vec![],
                    env: Default::default(),
                    timeout_secs: None,
                    output_max_bytes: None,
                    network: false,
                },
                VerifierSpec::PathsExist {
                    id: "p".into(),
                    paths: vec![],
                },
                VerifierSpec::ContentContains {
                    id: "bad-cc".into(),
                    path: String::new(),
                    must_include: vec!["something".into()],
                },
                VerifierSpec::GitNonemptyDiff { id: "diff".into() },
            ],
            out_of_scope: vec![],
            assumed_defaults: vec![],
            domain_hint: None,
            verify_profile: None,
        };
        sanitize_draft(&mut draft);
        assert_eq!(draft.verifiers.len(), 1);
        assert_eq!(draft.verifiers[0].id(), "diff");
    }

    #[test]
    fn validate_draft_content_contains_needs_fields() {
        let draft = GoalContractDraft {
            description: "x".into(),
            success_criteria: vec![],
            verifiers: vec![VerifierSpec::ContentContains {
                id: "c".into(),
                path: String::new(),
                must_include: vec![String::new()],
            }],
            out_of_scope: vec![],
            assumed_defaults: vec![],
            domain_hint: None,
            verify_profile: None,
        };
        assert!(validate_draft(&draft).is_err());
    }

    #[test]
    fn node_test_profile_resolves() {
        let profile = profile_verifiers("node-test");
        assert!(profile.iter().any(|v| v.id() == "npm-test"));
    }

    /// Dogfood finding #2: DeepSeek under json_object fallback emitted `prompt` as a sequence.
    #[test]
    fn question_prompt_accepts_string_array() {
        let raw = r#"{
            "status": "needs_clarification",
            "questions": [
                {
                    "id": "workspace_path",
                    "prompt": ["What is the absolute path?", "Please provide it."],
                    "options": ["a", "b"]
                }
            ]
        }"#;
        let outcome: IntakeOutcome = serde_json::from_str(raw).unwrap();
        match outcome {
            IntakeOutcome::NeedsClarification { questions, .. } => {
                assert_eq!(questions.len(), 1);
                assert!(questions[0].prompt.contains("absolute path"));
                assert!(questions[0].prompt.contains("Please provide"));
                assert_eq!(questions[0].options, vec!["a", "b"]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn profile_verifiers_known_profiles() {
        assert!(!profile_verifiers("rust-check").is_empty());
        assert!(!profile_verifiers("rust-strict").is_empty());
        assert!(!profile_verifiers("node-test").is_empty());
    }

    #[test]
    fn profile_verifiers_unknown_returns_empty() {
        assert!(profile_verifiers("").is_empty());
        assert!(profile_verifiers("bogus").is_empty());
        assert!(profile_verifiers("  unknown  ").is_empty());
    }

    #[test]
    fn sanitize_draft_drops_empty_command_verifier() {
        let mut draft = GoalContractDraft {
            description: "test".into(),
            success_criteria: vec![],
            verifiers: vec![
                VerifierSpec::Command {
                    id: "cmd".into(),
                    program: String::new(),
                    args: vec![],
                    env: Default::default(),
                    timeout_secs: None,
                    output_max_bytes: None,
                    network: false,
                },
                VerifierSpec::Command {
                    id: "ok".into(),
                    program: "echo".into(),
                    args: vec![],
                    env: Default::default(),
                    timeout_secs: None,
                    output_max_bytes: None,
                    network: false,
                },
            ],
            out_of_scope: vec![],
            assumed_defaults: vec![],
            domain_hint: None,
            verify_profile: None,
        };
        sanitize_draft(&mut draft);
        assert_eq!(draft.verifiers.len(), 1, "empty program should be dropped");
        assert_eq!(draft.verifiers[0].id(), "ok");
    }

    #[test]
    fn sanitize_draft_drops_paths_exist_with_no_paths() {
        let mut draft = GoalContractDraft {
            description: "test".into(),
            success_criteria: vec![],
            verifiers: vec![VerifierSpec::PathsExist {
                id: "pe".into(),
                paths: vec![],
            }],
            out_of_scope: vec![],
            assumed_defaults: vec![],
            domain_hint: None,
            verify_profile: None,
        };
        sanitize_draft(&mut draft);
        assert!(draft.verifiers.is_empty());
    }

    #[test]
    fn sanitize_draft_drops_paths_absent_with_no_paths() {
        let mut draft = GoalContractDraft {
            description: "test".into(),
            success_criteria: vec![],
            verifiers: vec![VerifierSpec::PathsAbsent {
                id: "pa".into(),
                paths: vec![],
            }],
            out_of_scope: vec![],
            assumed_defaults: vec![],
            domain_hint: None,
            verify_profile: None,
        };
        sanitize_draft(&mut draft);
        assert!(draft.verifiers.is_empty());
    }

    #[test]
    fn sanitize_draft_drops_content_contains_with_empty_path() {
        let mut draft = GoalContractDraft {
            description: "test".into(),
            success_criteria: vec![],
            verifiers: vec![VerifierSpec::ContentContains {
                id: "cc".into(),
                path: String::new(),
                must_include: vec!["needed".into()],
            }],
            out_of_scope: vec![],
            assumed_defaults: vec![],
            domain_hint: None,
            verify_profile: None,
        };
        sanitize_draft(&mut draft);
        assert!(draft.verifiers.is_empty());
    }

    #[test]
    fn sanitize_draft_drops_content_contains_with_empty_must_include() {
        let mut draft = GoalContractDraft {
            description: "test".into(),
            success_criteria: vec![],
            verifiers: vec![VerifierSpec::ContentContains {
                id: "cc".into(),
                path: "README.md".into(),
                must_include: vec![],
            }],
            out_of_scope: vec![],
            assumed_defaults: vec![],
            domain_hint: None,
            verify_profile: None,
        };
        sanitize_draft(&mut draft);
        assert!(
            draft.verifiers.is_empty(),
            "verifier with non-empty path but empty must_include should be dropped"
        );
    }

    #[test]
    fn sanitize_draft_keeps_git_diff_verifier() {
        let mut draft = GoalContractDraft {
            description: "test".into(),
            success_criteria: vec![],
            verifiers: vec![VerifierSpec::GitNonemptyDiff { id: "diff".into() }],
            out_of_scope: vec![],
            assumed_defaults: vec![],
            domain_hint: None,
            verify_profile: None,
        };
        sanitize_draft(&mut draft);
        assert_eq!(draft.verifiers.len(), 1);
    }

    #[test]
    fn validate_draft_rejects_content_contains_without_must_include() {
        let draft = GoalContractDraft {
            description: "test".into(),
            success_criteria: vec![],
            verifiers: vec![VerifierSpec::ContentContains {
                id: "cc".into(),
                path: "README.md".into(),
                must_include: vec![],
            }],
            out_of_scope: vec![],
            assumed_defaults: vec![],
            domain_hint: None,
            verify_profile: None,
        };
        assert!(validate_draft(&draft).is_err());
    }

    #[test]
    fn apply_to_request_populates_task_and_config() {
        let now = chrono::Utc::now();
        let draft = GoalContractDraft {
            description: "add feature".into(),
            success_criteria: vec!["test passes".into()],
            verifiers: vec![VerifierSpec::GitNonemptyDiff { id: "diff".into() }],
            out_of_scope: vec!["no db".into()],
            assumed_defaults: vec!["Rust".into()],
            domain_hint: None,
            verify_profile: None,
        };
        let content_hash = hash_draft(&draft);
        let contract = GoalContract {
            id: "g1".into(),
            draft,
            frozen_at: now,
            frozen_by: FreezeAuthority::Human,
            content_hash,
        };
        let empty_role = crate::CoderRoleConfig {
            model: String::new(),
            prompt_path: None,
            prompt: None,
            temperature: None,
            max_tokens: None,
            max_turns: None,
            reasoning: None,
        };
        let mut request = crate::CoderRunRequest {
            task: crate::CoderTask::new("x", "old"),
            workspace: crate::WorkspaceRef::new("/tmp", "main"),
            config: crate::CoderRunConfig {
                backend: String::new(),
                trace_dir: None,
                trace_formats: Vec::new(),
                planner: empty_role.clone(),
                coder: empty_role.clone(),
                critic: empty_role.clone(),
                gate: Default::default(),
                repair: None,
                sandbox: Default::default(),
                command_policy: Default::default(),
                validation_command: Some(crate::CoderCommandConfig::new("legacy")),
                verifiers: vec![],
                verify_policy: Default::default(),
                path_policy: Default::default(),
                progress: Default::default(),
                hashline: Default::default(),
                session_critic: crate::SessionCriticConfig::default(),
                prompt_dir: None,
                edit: Default::default(),
                workspace_build: Default::default(),
                offered_tools: None,
            },
            attempt: 0,
            prior_feedback: vec![],
            strategist_directive: None,
        };
        contract.apply_to_request(&mut request);
        assert_eq!(request.task.description, "add feature");
        assert_eq!(request.task.success_criteria, vec!["test passes"]);
        assert_eq!(request.config.verifiers.len(), 1);
        assert_eq!(request.config.verifiers[0].id(), "diff");
        assert!(
            request.config.validation_command.is_none(),
            "validation_command should be cleared when verifiers present"
        );
    }

    #[test]
    fn intake_outcome_schema_has_expected_shape() {
        let schema = intake_outcome_schema();
        assert_eq!(schema["type"], "object");
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "status"));
        let props = &schema["properties"];
        assert!(
            props["status"]["enum"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("needs_clarification"))
        );
    }
}

#[cfg(test)]
mod survivor_tests {
    //! Every visitor method of the two flexible deserializers, pinned through real JSON.
    use super::*;
    use serde_json::json;

    #[test]
    fn flexible_string_accepts_every_scalar_and_joins_sequences() {
        let q: IntakeQuestion = serde_json::from_value(json!({
            "id": 7,
            "prompt": ["line one", "line two", 3, null],
            "options": "single",
            "affects": true,
        }))
        .expect("scalars coerce");
        assert_eq!(q.id, "7");
        assert_eq!(q.prompt, "line one\nline two\n3");
        assert_eq!(q.options, vec!["single"]);
        assert_eq!(q.affects, "true");

        // Negative integers take the i64 arm; plain strings the str arm.
        let q: IntakeQuestion = serde_json::from_value(json!({
            "id": -5,
            "prompt": "plain text",
            "affects": "also plain",
        }))
        .unwrap();
        assert_eq!(q.id, "-5");
        assert_eq!(q.prompt, "plain text");
        assert_eq!(q.affects, "also plain");

        // f64 and explicit null paths.
        let q: IntakeQuestion = serde_json::from_value(json!({
            "id": 2.5,
            "prompt": null,
        }))
        .unwrap();
        assert_eq!(q.id, "2.5");
        assert_eq!(q.prompt, "");

        // Empty sequence joins to empty; nested values stringify compactly.
        let q: IntakeQuestion = serde_json::from_value(json!({
            "id": [],
            "prompt": [{"k": 1}],
            "options": [null, 4.5],
        }))
        .unwrap();
        assert_eq!(q.id, "");
        assert_eq!(q.prompt, "{\"k\":1}");
        assert_eq!(q.options, vec!["4.5"]);
    }

    #[test]
    fn string_or_vec_accepts_string_list_map_null_and_coerces_members() {
        let d: GoalContractDraft = serde_json::from_value(json!({
            "description": "d",
            "success_criteria": "one",
            "out_of_scope": ["a", "b"],
            "assumed_defaults": {"k1": "v1", "k2": true},
        }))
        .unwrap();
        assert_eq!(d.success_criteria, vec!["one"]);
        assert_eq!(d.out_of_scope, vec!["a", "b"]);
        // Map form keeps values, drops keys.
        assert_eq!(d.assumed_defaults, vec!["v1", "true"]);

        let d: GoalContractDraft = serde_json::from_value(json!({
            "description": "d",
            "success_criteria": [42, -3, true, null, {"o": []}],
            "verifiers": [],
        }))
        .unwrap();
        assert_eq!(
            d.success_criteria,
            vec![
                "42".to_string(),
                "-3".to_string(),
                "true".to_string(),
                "{\"o\":[]}".to_string()
            ]
        );

        // Whitespace-only string is an EMPTY list, not a one-element list.
        let d: GoalContractDraft =
            serde_json::from_value(json!({"description": "d", "out_of_scope": "   "})).unwrap();
        assert!(d.out_of_scope.is_empty());
    }

    #[test]
    fn wrong_container_types_fail_with_the_documented_expectation() {
        // An object where a flexible string is expected cannot be coerced — the error
        // text must name what was wanted, or triage loses its only hint.
        let err = serde_json::from_value::<IntakeQuestion>(json!({
            "id": {"nope": true},
            "prompt": "p",
        }))
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("a string, sequence of strings, or scalar"),
            "{err}"
        );

        let err = serde_json::from_value::<GoalContractDraft>(json!({
            "description": "d",
            "success_criteria": 1.5,
        }))
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("a string, sequence of strings, or empty"),
            "{err}"
        );
    }
}
