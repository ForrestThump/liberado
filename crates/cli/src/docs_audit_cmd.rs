//! Deterministic documentation contracts and change-impact checks.

use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[path = "docs_audit_cmd/change_set.rs"]
mod change_set;

const POLICY: &str = "docs-audit.toml";

#[derive(Debug, Deserialize)]
struct Policy {
    #[serde(default)]
    contract: Vec<Contract>,
    #[serde(default)]
    impact: Vec<Impact>,
    #[serde(default)]
    waiver: Vec<Waiver>,
    #[serde(default)]
    obsolete: Vec<Obsolete>,
}

#[derive(Debug, Deserialize)]
struct Contract {
    name: String,
    source: String,
    document: String,
    #[serde(default)]
    source_terms: Vec<String>,
    #[serde(default)]
    document_terms: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Impact {
    name: String,
    sources: Vec<String>,
    documents: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Waiver {
    source: String,
    reason: String,
    reviewed_on: String,
}

#[derive(Debug, Deserialize)]
struct Obsolete {
    term: String,
    replacement: String,
}

pub fn run(
    root: &Path,
    args: impl Iterator<Item = String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let base = parse_base(args)?;
    let policy = load_policy(root)?;
    audit_static(root, &policy)?;
    audit_impact(root, &policy, base)?;
    println!("docs audit: OK");
    Ok(())
}

fn parse_base(
    args: impl Iterator<Item = String>,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let arguments: Vec<_> = args.collect();
    Ok(match arguments.as_slice() {
        [] => None,
        [flag, base] if flag == "--base" => Some(base.clone()),
        _ => return Err("usage: liberado docs audit [--base <git-revision>]".into()),
    })
}

fn audit_static(root: &Path, policy: &Policy) -> Result<(), Box<dyn std::error::Error>> {
    validate_policy(root, policy)?;
    check_contracts(root, policy)?;
    check_examples(root)?;
    check_obsolete_terms(root, policy)
}

fn audit_impact(
    root: &Path,
    policy: &Policy,
    base: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(base) = base {
        check_impact(policy, &changed_files(root, &base)?)?;
    }
    Ok(())
}

fn load_policy(root: &Path) -> Result<Policy, Box<dyn std::error::Error>> {
    Ok(toml::from_str(&fs::read_to_string(root.join(POLICY))?)?)
}

fn validate_policy(root: &Path, policy: &Policy) -> Result<(), Box<dyn std::error::Error>> {
    validate_contracts(root, &policy.contract)?;
    validate_impacts(root, &policy.impact)?;
    validate_waivers(root, &policy.waiver)
}

fn validate_contracts(
    root: &Path,
    contracts: &[Contract],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut names = BTreeSet::new();
    for contract in contracts {
        if !names.insert(&contract.name) {
            return Err(format!("duplicate docs contract: {}", contract.name).into());
        }
        require_file(root, &contract.source)?;
        require_file(root, &contract.document)?;
    }
    Ok(())
}

fn validate_impacts(root: &Path, impacts: &[Impact]) -> Result<(), Box<dyn std::error::Error>> {
    let mut names = BTreeSet::new();
    for impact in impacts {
        if !names.insert(&impact.name) {
            return Err(format!("duplicate docs impact rule: {}", impact.name).into());
        }
        if impact.sources.is_empty() || impact.documents.is_empty() {
            return Err(format!(
                "docs impact rule {} must name sources and documents",
                impact.name
            )
            .into());
        }
        for document in &impact.documents {
            require_file(root, document)?;
        }
    }
    Ok(())
}

fn validate_waivers(root: &Path, waivers: &[Waiver]) -> Result<(), Box<dyn std::error::Error>> {
    for waiver in waivers {
        require_file(root, &waiver.source)?;
        if waiver.reason.trim().is_empty() || waiver.reviewed_on.trim().is_empty() {
            return Err(format!(
                "docs waiver for {} needs reason and reviewed_on",
                waiver.source
            )
            .into());
        }
    }
    Ok(())
}

fn require_file(root: &Path, path: &str) -> Result<(), Box<dyn std::error::Error>> {
    if root.join(path).is_file() {
        Ok(())
    } else {
        Err(format!("docs audit path does not exist: {path}").into())
    }
}

fn check_contracts(root: &Path, policy: &Policy) -> Result<(), Box<dyn std::error::Error>> {
    for contract in &policy.contract {
        let source = read_text(&root.join(&contract.source))?;
        let document = read_text(&root.join(&contract.document))?;
        require_terms(
            &contract.name,
            &contract.source,
            &source,
            &contract.source_terms,
        )?;
        require_terms(
            &contract.name,
            &contract.document,
            &document,
            &contract.document_terms,
        )?;
    }
    Ok(())
}

fn require_terms(
    name: &str,
    path: &str,
    text: &str,
    terms: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let missing: Vec<_> = terms
        .iter()
        .filter(|term| !text.contains(term.as_str()))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("docs contract {name} is missing {missing:?} in {path}").into())
    }
}

fn changed_files(root: &Path, base: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    change_set::changed_files(root, base)
}

fn check_impact(policy: &Policy, changed: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let waived: BTreeSet<_> = policy
        .waiver
        .iter()
        .map(|waiver| waiver.source.as_str())
        .collect();
    let mut failures = Vec::new();
    for rule in &policy.impact {
        let sources: Vec<_> = changed
            .iter()
            .filter(|path| matches_prefix(path, &rule.sources))
            .collect();
        if sources.is_empty() || changed.iter().any(|path| rule.documents.contains(path)) {
            continue;
        }
        let unwaived: Vec<_> = sources
            .into_iter()
            .filter(|path| !waived.contains(path.as_str()))
            .collect();
        if !unwaived.is_empty() {
            failures.push(format!(
                "{}: changed {:?}; update one of {:?}",
                rule.name, unwaived, rule.documents
            ));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("documentation impact review required:\n{}\nUse a narrow [[waiver]] only after human review.", failures.join("\n")).into())
    }
}

fn matches_prefix(path: &str, prefixes: &[String]) -> bool {
    prefixes
        .iter()
        .any(|prefix| path == prefix || path.starts_with(prefix))
}

fn check_examples(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut docs = Vec::new();
    collect_markdown(&root.join("docs"), &mut docs)?;
    for path in docs {
        check_document_examples(&path)?;
    }
    Ok(())
}

fn check_document_examples(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    for (language, body) in checked_fences(&read_text(path)?) {
        parse_example(language, &body)?;
    }
    Ok(())
}

fn parse_example(language: &str, body: &str) -> Result<(), Box<dyn std::error::Error>> {
    match language {
        "toml" => {
            toml::from_str::<toml::Value>(body)?;
        }
        "json" => {
            serde_json::from_str::<serde_json::Value>(body)?;
        }
        "yaml" => {
            serde_yaml::from_str::<serde_yaml::Value>(body)?;
        }
        _ => unreachable!("checked_fences returns supported languages"),
    }
    Ok(())
}

fn checked_fences(text: &str) -> Vec<(&str, String)> {
    let mut result = Vec::new();
    let mut current: Option<(&str, String)> = None;
    for line in text.lines() {
        if let Some(language) = line.strip_prefix("```").map(str::trim) {
            if let Some((active, body)) = current.take() {
                result.push((active, body));
            } else if let Some(language) = language.strip_suffix(" check")
                && ["toml", "json", "yaml"].contains(&language)
            {
                current = Some((language, String::new()));
            }
        } else if let Some((_, body)) = current.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    result
}

fn check_obsolete_terms(root: &Path, policy: &Policy) -> Result<(), Box<dyn std::error::Error>> {
    let mut docs = Vec::new();
    collect_markdown(&root.join("docs"), &mut docs)?;
    for path in docs.into_iter().filter(|path| {
        !path
            .to_string_lossy()
            .replace('\\', "/")
            .contains("/archive/")
    }) {
        let text = read_text(&path)?;
        for obsolete in &policy.obsolete {
            if text.to_lowercase().contains(&obsolete.term.to_lowercase()) {
                return Err(format!(
                    "{} uses obsolete term {:?}; use {:?}",
                    path.display(),
                    obsolete.term,
                    obsolete.replacement
                )
                .into());
            }
        }
    }
    Ok(())
}

fn collect_markdown(dir: &Path, paths: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_markdown(&path, paths)?;
        } else if path.extension().and_then(|value| value.to_str()) == Some("md") {
            paths.push(path);
        }
    }
    Ok(())
}

fn read_text(path: &Path) -> std::io::Result<String> {
    let bytes = fs::read(path)?;
    Ok(String::from_utf8_lossy(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes)).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn checked_examples_are_opt_in_and_language_specific() {
        let fences = checked_fences("```toml check\na = 1\n```\n```rust\nnope\n```\n");
        assert_eq!(fences, vec![("toml", "a = 1\n".to_owned())]);
    }

    #[test]
    fn impact_requires_a_matching_document_or_exact_waiver() {
        let policy = Policy {
            contract: vec![],
            obsolete: vec![],
            impact: vec![Impact {
                name: "config".into(),
                sources: vec!["src/config/".into()],
                documents: vec![format!("docs/{}.md", "config")],
            }],
            waiver: vec![],
        };
        assert!(check_impact(&policy, &["src/config/key.rs".into()]).is_err());
        assert!(
            check_impact(
                &policy,
                &["src/config/key.rs".into(), format!("docs/{}.md", "config")]
            )
            .is_ok()
        );
    }

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
}
