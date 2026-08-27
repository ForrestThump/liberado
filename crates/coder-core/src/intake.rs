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
#[path = "intake_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "intake_survivor_tests.rs"]
mod survivor_tests;
