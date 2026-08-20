use regex::Regex;
use serde_yaml::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
struct Document {
    path: String,
    meta: Option<serde_yaml::Mapping>,
    body: String,
}

#[derive(Debug)]
struct Issue {
    path: String,
    message: String,
}

fn read_text(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;
    Ok(String::from_utf8_lossy(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes)).into())
}

fn split_frontmatter(text: &str) -> (Option<serde_yaml::Mapping>, String) {
    let normalized = text.replace("\r\n", "\n");
    if !normalized.starts_with("---\n") {
        return (None, normalized);
    }
    let Some(end) = normalized[4..].find("\n---\n") else {
        return (None, normalized);
    };
    let yaml = &normalized[4..4 + end];
    let body = normalized[4 + end + 5..].to_owned();
    let meta = serde_yaml::from_str::<Value>(yaml)
        .ok()
        .and_then(|v| v.as_mapping().cloned());
    (meta, body)
}

fn load_docs(root: &Path) -> Result<Vec<Document>, Box<dyn std::error::Error>> {
    let mut paths = Vec::new();
    collect_markdown(&root.join("docs"), &mut paths)?;
    paths = crate::docs_cmd::retain_unignored_files(root, paths);
    paths.sort();
    let mut docs = Vec::new();
    for path in paths {
        let text = read_text(&path)?;
        let (meta, body) = split_frontmatter(&text);
        docs.push(Document {
            path: path
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/"),
            meta,
            body,
        });
    }
    Ok(docs)
}

fn collect_markdown(dir: &Path, paths: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_markdown(&path, paths)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            paths.push(path);
        }
    }
    Ok(())
}

fn is_root_future_work(path: &str) -> bool {
    let mut parts = path.split('/');
    matches!((parts.next(), parts.next(), parts.next(), parts.next()),
        (Some("docs"), Some("future-work"), Some(name), None) if name.ends_with(".md") && name != "README.md")
}

fn is_managed(path: &str, meta: &Option<serde_yaml::Mapping>) -> bool {
    path.starts_with("docs/")
        && path.ends_with(".md")
        && (is_root_future_work(path) || meta.is_some())
}

fn meta_str<'a>(meta: &'a serde_yaml::Mapping, name: &str) -> Option<&'a str> {
    meta.get(Value::String(name.to_owned()))
        .and_then(Value::as_str)
}

fn meta_bool(meta: &serde_yaml::Mapping, name: &str) -> Option<bool> {
    meta.get(Value::String(name.to_owned()))
        .and_then(Value::as_bool)
}

fn display_meta(meta: &serde_yaml::Mapping, name: &str) -> String {
    meta_str(meta, name).unwrap_or("—").to_owned()
}

fn parse_active_links(text: &str) -> HashSet<String> {
    let section = Regex::new(r"(?is)## Active (?:documents|plans).*?\n(.*?)(?:\n## |\z)")
        .unwrap()
        .captures(text)
        .map(|c| c[1].to_owned())
        .unwrap_or_else(|| text.to_owned());
    let links = Regex::new(r"\]\(([^)]+\.md)\)").unwrap();
    links
        .captures_iter(&section)
        .filter_map(|c| {
            let target = &c[1];
            if target.starts_with("http") || target.contains("archive/") {
                return None;
            }
            Some(target.rsplit('/').next().unwrap_or(target).to_owned())
        })
        .collect()
}

fn generate_future_work_readme(docs: &[Document]) -> String {
    let mut active = Vec::new();
    let mut other = Vec::new();
    for doc in docs {
        if !is_root_future_work(&doc.path) {
            continue;
        }
        let Some(meta) = &doc.meta else { continue };
        let name = Path::new(&doc.path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if meta_str(meta, "status") == Some("active") {
            active.push((name, meta));
        } else {
            other.push((name, meta));
        }
    }
    active.sort_by(|a, b| a.0.cmp(&b.0));
    other.sort_by(|a, b| a.0.cmp(&b.0));
    let mut lines = vec![
        "---", "kind: index", "status: active", "authority: advisory", "generated: true", "---", "",
        "# Future Work", "", "Index of forward-looking work. **Generated** by `liberado docs metadata generate`.",
        "Do not edit the tables by hand — update document frontmatter and re-run generate.", "",
        "| Doc | Role |", "|-----|------|", "| **[roadmap.md](../roadmap.md)** | **Living scoreboard** — open work in priority order |",
        "| [backlog.md](backlog.md) | **Pick-from-here backlog** — only place agents should take next implementation items |",
        "| [archive/](archive/README.md) | Finished plans, closed audits — **not current truth** |",
        "| [CATALOG.md](../CATALOG.md) | Repository-wide document catalog |", "", "## Active documents", "",
        "Only root documents with `status: active` appear here. This includes active plans,",
        "ongoing findings, and current evidence. Implemented and superseded plans are archived.", "",
        "| Document | Kind | Domain | Authority |", "|------|------|--------|-----------|",
    ].into_iter().map(str::to_owned).collect::<Vec<_>>();
    if active.is_empty() {
        lines.push("| *(none)* | | | |".to_owned());
    } else {
        for (name, meta) in active {
            lines.push(format!(
                "| [{name}]({name}) | {} | {} | {} |",
                display_meta(meta, "kind"),
                display_meta(meta, "domain"),
                display_meta(meta, "authority")
            ));
        }
    }
    lines.extend(["", "## Non-active root documents", "", "Root files that are not active (historical findings kept briefly, or pending archive).", "Prefer archive/ for completed plans.", "", "| Doc | Status | Kind |", "|-----|--------|------|"].into_iter().map(str::to_owned));
    if other.is_empty() {
        lines.push("| *(none)* | | |".to_owned());
    } else {
        for (name, meta) in other {
            lines.push(format!(
                "| [{name}]({name}) | {} | {} |",
                display_meta(meta, "status"),
                display_meta(meta, "kind")
            ));
        }
    }
    lines.extend(
        [
            "",
            "Start every planning session at [roadmap.md](../roadmap.md).",
            "",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    lines.join("\n")
}

fn generate_catalog(docs: &[Document]) -> String {
    let mut rows: Vec<&Document> = docs.iter().filter(|d| d.meta.is_some()).collect();
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    let mut lines = vec![
        "---",
        "kind: index",
        "status: active",
        "authority: advisory",
        "generated: true",
        "---",
        "",
        "# Document catalog",
        "",
        "Repository-wide catalog of managed documents (those with YAML frontmatter).",
        "**Generated** by `liberado docs metadata generate`. Do not edit by hand.",
        "",
        "Authority model: [doc-authority.md](spec/reference/doc-authority.md).",
        "",
        "| Path | Kind | Status | Authority | Domain | Canonical for |",
        "|------|------|--------|-----------|--------|---------------|",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    for doc in rows {
        let meta = doc.meta.as_ref().unwrap();
        let href = doc.path.strip_prefix("docs/").unwrap_or(&doc.path);
        lines.push(format!(
            "| [{}]({}) | {} | {} | {} | {} | {} |",
            doc.path,
            href,
            display_meta(meta, "kind"),
            display_meta(meta, "status"),
            display_meta(meta, "authority"),
            display_meta(meta, "domain"),
            display_meta(meta, "canonical_for")
        ));
    }
    lines.push(String::new());
    lines.join("\n")
}

fn exact_file(root: &Path, rel: &str) -> bool {
    let mut current = root.to_path_buf();
    for part in rel.replace('\\', "/").trim_matches('/').split('/') {
        if !current.is_dir() || !current.join(part).exists() {
            return false;
        }
        let Ok(found) =
            fs::read_dir(&current).map(|entries| entries.flatten().any(|e| e.file_name() == part))
        else {
            return false;
        };
        if !found {
            return false;
        }
        current.push(part);
    }
    current.is_file()
}

fn stale_rs_paths(root: &Path) -> Result<Vec<Issue>, Box<dyn std::error::Error>> {
    let mut paths = Vec::new();
    collect_rs(&root.join("crates"), &mut paths)?;
    let reference = Regex::new(r"(?:^|[^\w./-])(docs/(?:[\w.-]+/)*[\w.-]+\.md)")?;
    let mut issues = Vec::new();
    for path in paths {
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let rel = path
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        for (line_no, line) in text.lines().enumerate() {
            // Test fixtures often use plausible docs paths without creating files. Mark those
            // literals explicitly instead of weakening the check for real Rust references.
            if line.contains("docs-check: ignore") {
                continue;
            }
            for prefix in [
                concat!("docs/", "architecture/"),
                concat!("docs/", "roadmap/"),
            ] {
                if line.contains(prefix) {
                    issues.push(Issue {
                        path: rel.clone(),
                        message: format!("obsolete prefix {prefix}: {}", line.trim()),
                    });
                }
            }
            for m in reference.captures_iter(line) {
                let target = &m[1];
                if !exact_file(root, target) {
                    issues.push(Issue {
                        path: format!("{}:{}", rel, line_no + 1),
                        message: format!("missing docs path {target}: {}", line.trim()),
                    });
                }
            }
        }
    }
    Ok(issues)
}

fn collect_rs(dir: &Path, paths: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_rs(&path, paths)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            paths.push(path);
        }
    }
    Ok(())
}

fn lint(root: &Path) -> Result<Vec<Issue>, Box<dyn std::error::Error>> {
    let docs = load_docs(root)?;
    let readme = root.join("docs/future-work/README.md");
    let active = if readme.is_file() {
        parse_active_links(&read_text(&readme)?)
    } else {
        HashSet::new()
    };
    let generated = [
        (
            "docs/future-work/README.md",
            generate_future_work_readme(&docs),
        ),
        ("docs/CATALOG.md", generate_catalog(&docs)),
    ];
    let archive_link = Regex::new(r"\]\(((?:\.\./)*(?:future-work/)?archive/[^)#\s]+)\)")?;
    let mut issues = Vec::new();
    let mut canonical = HashMap::new();
    for doc in &docs {
        issues.extend(lint_doc(doc, &active, &mut canonical, &archive_link));
    }
    for (name, expected) in generated {
        check_generated_index(root, name, &expected, &mut issues)?;
    }
    Ok(issues)
}

/// Run every metadata rule against one document.
fn lint_doc(
    doc: &Document,
    active: &HashSet<String>,
    canonical: &mut HashMap<String, String>,
    archive_link: &Regex,
) -> Vec<Issue> {
    let Some(meta) = doc.meta.as_ref() else {
        // A root future-work document without frontmatter is a real gap; anything else
        // unmanaged is skipped.
        return if is_root_future_work(&doc.path) {
            vec![Issue {
                path: doc.path.clone(),
                message: "root future-work document missing YAML frontmatter metadata".into(),
            }]
        } else {
            Vec::new()
        };
    };
    if !is_managed(&doc.path, &doc.meta) {
        return Vec::new();
    }
    let mut issues = lint_required_fields(doc, meta);
    issues.extend(lint_meta_rules(doc, meta));
    if let Some(issue) = lint_finished_plan_in_index(doc, meta, active) {
        issues.push(issue);
    }
    if let Some(issue) = lint_canonical_for(doc, meta, canonical) {
        issues.push(issue);
    }
    if let Some(issue) = lint_normative_archive(doc, meta, archive_link) {
        issues.push(issue);
    }
    issues
}

/// The kind/status/authority fields every managed document must carry.
fn lint_required_fields(doc: &Document, meta: &serde_yaml::Mapping) -> Vec<Issue> {
    let mut issues = Vec::new();
    for field in ["kind", "status", "authority"] {
        if !meta.contains_key(Value::String(field.into())) {
            issues.push(Issue {
                path: doc.path.clone(),
                message: format!("missing required metadata field: {field}"),
            });
        }
    }
    issues
}

/// Vocabulary and cross-field rules for a managed document's metadata.
fn lint_meta_rules(doc: &Document, meta: &serde_yaml::Mapping) -> Vec<Issue> {
    let kind = meta_str(meta, "kind").unwrap_or("");
    let status = meta_str(meta, "status").unwrap_or("");
    let authority = meta_str(meta, "authority").unwrap_or("");
    let mut issues = Vec::new();
    if ![
        "architecture",
        "reference",
        "decision",
        "plan",
        "finding",
        "validation",
        "runbook",
        "index",
        "policy",
        "",
    ]
    .contains(&kind)
    {
        issues.push(Issue {
            path: doc.path.clone(),
            message: format!("invalid kind '{kind}'"),
        });
    }
    if !["normative", "implementation", "advisory", "evidence", ""].contains(&authority) {
        issues.push(Issue {
            path: doc.path.clone(),
            message: format!("invalid authority '{authority}'"),
        });
    }
    let valid_status = if kind == "decision" {
        [
            "draft",
            "proposed",
            "accepted",
            "superseded",
            "historical",
            "",
        ]
        .contains(&status)
    } else {
        [
            "draft",
            "active",
            "implemented",
            "superseded",
            "historical",
            "",
        ]
        .contains(&status)
    };
    if !valid_status {
        issues.push(Issue {
            path: doc.path.clone(),
            message: format!("invalid status '{status}' for kind '{kind}'"),
        });
    }
    if status == "active" && kind == "plan" && meta_bool(meta, "open_items") != Some(true) {
        issues.push(Issue {
            path: doc.path.clone(),
            message: "active plan must set open_items: true (completed slices belong elsewhere)"
                .into(),
        });
    }
    issues
}

/// A finished plan must not stay in the active future-work index.
fn lint_finished_plan_in_index(
    doc: &Document,
    meta: &serde_yaml::Mapping,
    active: &HashSet<String>,
) -> Option<Issue> {
    let kind = meta_str(meta, "kind").unwrap_or("");
    let status = meta_str(meta, "status").unwrap_or("");
    let file = Path::new(&doc.path).file_name()?.to_str()?.to_string();
    if kind == "plan" && ["implemented", "superseded"].contains(&status) && active.contains(&file) {
        Some(Issue {
            path: doc.path.clone(),
            message: format!("{status} plan must not appear in the active future-work index"),
        })
    } else {
        None
    }
}

/// Two active documents must not claim the same canonical_for.
fn lint_canonical_for(
    doc: &Document,
    meta: &serde_yaml::Mapping,
    canonical: &mut HashMap<String, String>,
) -> Option<Issue> {
    let status = meta_str(meta, "status").unwrap_or("");
    if ["active", "accepted", "proposed"].contains(&status)
        && let Some(canon) = meta_str(meta, "canonical_for")
        && let Some(previous) = canonical.insert(canon.to_owned(), doc.path.clone())
    {
        Some(Issue {
            path: doc.path.clone(),
            message: format!(
                "duplicate active canonical_for '{canon}' (also claimed by {previous})"
            ),
        })
    } else {
        None
    }
}

/// A normative document must not link to an archive path as content.
fn lint_normative_archive(
    doc: &Document,
    meta: &serde_yaml::Mapping,
    archive_link: &Regex,
) -> Option<Issue> {
    let authority = meta_str(meta, "authority").unwrap_or("");
    (authority == "normative" && archive_link.is_match(&doc.body)).then(|| Issue {
        path: doc.path.clone(),
        message: "normative document links to archive path as content".into(),
    })
}

/// A generated index must match its committed copy.
fn check_generated_index(
    root: &Path,
    name: &str,
    expected: &str,
    issues: &mut Vec<Issue>,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = root.join(name);
    if !path.is_file() || read_text(&path)?.replace("\r\n", "\n") != expected {
        issues.push(Issue {
            path: name.into(),
            message: "generated index differs from committed copy (run generate and commit)".into(),
        });
    }
    Ok(())
}

/// Pure regression checks for the metadata rules. This is a command, rather than only Rust unit
/// tests, because CI invokes `liberado docs metadata self-test` before linting the repository.
/// Pure regression checks for the metadata rules. This is a command, rather than only Rust unit
/// tests, because CI invokes `liberado docs metadata self-test` before linting the repository.
fn self_test() -> Result<(), Box<dyn std::error::Error>> {
    assert_frontmatter_round_trip()?;
    assert_missing_future_work_frontmatter()?;
    assert_active_plan_open_items()?;
    assert_duplicate_canonical_for()?;
    assert_decision_status_vocabulary()?;
    assert_normative_archive_link()?;
    assert_generated_index_comparison()?;
    assert_exact_case()?;
    assert_no_stale_rs_docs()?;
    println!("docs metadata self-test: all passed");
    Ok(())
}

/// Fail the self-test run unless `passed` is true.
fn assert_rule(name: &str, passed: bool) -> Result<(), Box<dyn std::error::Error>> {
    if passed {
        Ok(())
    } else {
        Err(format!("docs metadata self-test failed: {name}").into())
    }
}

/// Build a `Document` from frontmatter YAML text and a body, for a self-test fixture.
fn test_document(path: &str, meta: &str, body: &str) -> Document {
    Document {
        path: path.into(),
        meta: split_frontmatter(&format!("---\n{meta}\n---\n{body}")).0,
        body: body.into(),
    }
}

/// frontmatter parses back to the same kind and body.
fn assert_frontmatter_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let (meta, body) = split_frontmatter("---\nkind: plan\nopen_items: true\n---\n# Title\n");
    assert_rule(
        "frontmatter round trip",
        meta.as_ref().and_then(|m| meta_str(m, "kind")) == Some("plan") && body == "# Title\n",
    )
}

/// A root future-work document without frontmatter is flagged.
fn assert_missing_future_work_frontmatter() -> Result<(), Box<dyn std::error::Error>> {
    let missing = lint_documents_for_test(vec![Document {
        path: format!("docs/{}/missing.md", "future-work"),
        meta: None,
        body: String::new(),
    }])?;
    assert_rule(
        "missing future-work frontmatter",
        has_issue(&missing, "missing YAML"),
    )
}

/// An active plan must keep open_items: true.
fn assert_active_plan_open_items() -> Result<(), Box<dyn std::error::Error>> {
    let inactive = lint_documents_for_test(vec![test_document(
        &format!("docs/{}/active.md", "future-work"),
        "kind: plan\nstatus: active\nauthority: implementation\nopen_items: false",
        "",
    )])?;
    assert_rule("active plan open_items", has_issue(&inactive, "open_items"))
}

/// Two active plans cannot claim the same canonical_for.
fn assert_duplicate_canonical_for() -> Result<(), Box<dyn std::error::Error>> {
    let duplicate = lint_documents_for_test(vec![
        test_document(
            &format!("docs/{}/a.md", "future-work"),
            "kind: plan\nstatus: active\nauthority: implementation\nopen_items: true\ncanonical_for: same",
            "",
        ),
        test_document(
            &format!("docs/{}/b.md", "future-work"),
            "kind: plan\nstatus: active\nauthority: implementation\nopen_items: true\ncanonical_for: same",
            "",
        ),
    ])?;
    assert_rule(
        "duplicate canonical_for",
        has_issue(&duplicate, "duplicate active canonical_for"),
    )
}

/// A decision must use the decision-status vocabulary.
fn assert_decision_status_vocabulary() -> Result<(), Box<dyn std::error::Error>> {
    let invalid_status = lint_documents_for_test(vec![test_document(
        &format!("docs/{}/ADR-0001.md", "decisions"),
        "kind: decision\nstatus: banana\nauthority: normative",
        "",
    )])?;
    assert_rule(
        "decision status vocabulary",
        has_issue(&invalid_status, "invalid status 'banana'"),
    )
}

/// A normative document must not link to an archive path as content.
fn assert_normative_archive_link() -> Result<(), Box<dyn std::error::Error>> {
    let archive = lint_documents_for_test(vec![test_document(
        "docs/spec/architecture/contracts.md",
        "kind: architecture\nstatus: active\nauthority: normative",
        "See [old](../future-work/archive/old.md).",
    )])?;
    assert_rule(
        "normative archive link",
        has_issue(&archive, "archive path"),
    )
}

/// A committed generated index must match what the generator would produce.
fn assert_generated_index_comparison() -> Result<(), Box<dyn std::error::Error>> {
    let generated = tempfile::tempdir()?;
    fs::create_dir_all(generated.path().join("docs").join("future-work"))?;
    fs::write(
        generated
            .path()
            .join("docs")
            .join("future-work")
            .join("example.md"),
        "---\nkind: plan\nstatus: active\nauthority: implementation\nopen_items: true\n---\n# Example\n",
    )?;
    fs::write(
        generated
            .path()
            .join("docs")
            .join("future-work")
            .join("README.md"),
        "stale\n",
    )?;
    fs::write(generated.path().join("docs/CATALOG.md"), "stale\n")?;
    assert_rule(
        "generated index comparison",
        has_issue(&lint(generated.path())?, "generated index differs"),
    )
}

/// exact_file is case-sensitive and leaf-exact.
fn assert_exact_case() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    fs::create_dir_all(root.join("docs/spec/reference"))?;
    fs::write(root.join("docs/spec/reference/api.md"), "# api\n")?;
    assert_rule(
        "exact case rejects wrong case",
        !exact_file(root, &format!("docs/spec/reference/{}", "API.md")),
    )?;
    assert_rule(
        "exact case accepts matching path",
        exact_file(root, "docs/spec/reference/api.md"),
    )
}

/// A rust doc link to a missing docs path is flagged.
fn assert_no_stale_rs_docs() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    fs::create_dir_all(root.join("crates/example/src"))?;
    fs::write(
        root.join("crates/example/src/lib.rs"),
        format!("//! See docs/{}/missing.md\n", "future-work"),
    )?;
    let stale = stale_rs_paths(root)?;
    assert_rule(
        "stale Rust docs path",
        has_issue(&stale, "missing docs path"),
    )
}

fn lint_documents_for_test(docs: Vec<Document>) -> Result<Vec<Issue>, Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    for doc in &docs {
        let path = temp.path().join(&doc.path);
        fs::create_dir_all(path.parent().expect("document has parent"))?;
        let text = match &doc.meta {
            Some(meta) => format!("---\n{}---\n{}", serde_yaml::to_string(meta)?, doc.body),
            None => doc.body.clone(),
        };
        fs::write(path, text)?;
    }
    fs::create_dir_all(temp.path().join("docs/future-work"))?;
    // The catalog lists generated documents too. Two passes reach the same fixed point as the
    // production generator.
    for _ in 0..2 {
        let loaded = load_docs(temp.path())?;
        fs::write(
            temp.path().join("docs/future-work/README.md"),
            generate_future_work_readme(&loaded),
        )?;
        fs::write(
            temp.path().join("docs/CATALOG.md"),
            generate_catalog(&loaded),
        )?;
    }
    lint(temp.path())
}

fn has_issue(issues: &[Issue], needle: &str) -> bool {
    issues.iter().any(|issue| issue.message.contains(needle))
}

pub fn run(root: &Path, command: &str) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        "self-test" => self_test(),
        "generate" => run_generate(root),
        "lint" => run_lint(root),
        "check-stale-rs" => run_check_stale_rs(root),
        _ => Err(format!("unknown docs metadata command: {command}").into()),
    }
}

/// Regenerate the future-work README and the catalog from the on-disk docs.
fn run_generate(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let docs = load_docs(root)?;
    fs::write(
        root.join("docs/future-work/README.md"),
        generate_future_work_readme(&docs),
    )?;
    fs::write(root.join("docs/CATALOG.md"), generate_catalog(&docs))?;
    println!("wrote docs/future-work/README.md");
    println!("wrote docs/CATALOG.md");
    Ok(())
}

/// Lint the docs metadata, printing the issue list on failure.
fn run_lint(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let issues = lint(root)?;
    if issues.is_empty() {
        println!("docs metadata lint: OK");
        Ok(())
    } else {
        for i in &issues {
            eprintln!("  {}: {}", i.path, i.message);
        }
        Err(format!("docs metadata lint failed ({} issue(s))", issues.len()).into())
    }
}

/// Scan Rust sources for stale docs paths, printing the offenders on failure.
fn run_check_stale_rs(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let issues = stale_rs_paths(root)?;
    if issues.is_empty() {
        println!(
            "stale-rs-paths: OK (no obsolete prefixes; all docs/*.md references in crates resolve on disk)"
        );
        Ok(())
    } else {
        for i in issues.iter().take(50) {
            eprintln!("  {}: {}", i.path, i.message);
        }
        Err(format!("stale-rs-paths: FAILED ({} issue(s))", issues.len()).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_test_exercises_metadata_rules() {
        self_test().unwrap();
    }
    #[test]
    fn run_dispatches_the_metadata_commands() {
        assert!(run(Path::new("."), "self-test").is_ok());
        assert!(run(Path::new("."), "frobnicate").is_err());
    }

    #[test]
    fn split_frontmatter_keeps_first_body_character() {
        let (_, body) = split_frontmatter("---\nkind: plan\n---\n# Hello\n");
        assert_eq!(body, "# Hello\n");
    }

    // ── is_root_future_work ─────────────────────────────────────────────

    #[test]
    fn root_future_work_is_a_root_markdown_leaf() {
        for tail in ["x", "plan-2026"] {
            // docs-check: ignore
            let yes = format!("docs/future-work/{tail}.md");
            assert!(is_root_future_work(&yes), "{yes}");
        }
        // Not a leaf, not markdown, or the auto-generated README itself.
        for no in [
            "docs/future-work/x/y.md",     // docs-check: ignore
            "docs/future-work/x/index.md", // docs-check: ignore
            "docs/future-work/README.md",
            "docs/future-work/x.txt",
            "docs/other/x.md", // docs-check: ignore
            "docs/future-work",
        ] {
            assert!(!is_root_future_work(no), "{no}");
        }
    }

    // ── is_managed ──────────────────────────────────────────────────────

    #[test]
    fn managed_is_a_docs_markdown_file_with_meta_or_root_plan() {
        let meta = yaml_map("kind: plan\n");
        assert!(is_managed("docs/goals.md", &Some(meta.clone()))); // docs-check: ignore
        assert!(is_managed("docs/future-work/plan.md", &None)); // docs-check: ignore
        assert!(!is_managed("docs/goals.md", &None)); // docs-check: ignore
        assert!(!is_managed("crates/x/readme.md", &Some(meta.clone())));
        assert!(!is_managed("docs/README.txt", &Some(meta)));
    }

    // ── meta accessors ──────────────────────────────────────────────────

    #[test]
    fn meta_accessors_read_and_default() {
        let meta = yaml_map("kind: plan\ndomain: \"\"\ncount: 3\n");
        assert_eq!(meta_str(&meta, "kind"), Some("plan"));
        assert_eq!(meta_str(&meta, "missing"), None);
        assert_eq!(display_meta(&meta, "kind"), "plan");
        assert_eq!(display_meta(&meta, "missing"), "—");
        assert_eq!(meta_bool(&meta, "count"), None, "non-bool is None");
        let bool_meta = yaml_map("flag: true\n");
        assert_eq!(meta_bool(&bool_meta, "flag"), Some(true));
    }

    // ── parse_active_links ──────────────────────────────────────────────

    /// Active-links scanning: pulls `]()` link targets out of the "## Active …" section, keeps
    /// basenames, and skips external URLs and archived plans.
    #[test]
    fn active_links_are_scanned_from_the_active_section() {
        let text = "# Index\n\n## Active documents\n\n- [A](a.md)\n- [B](sub/b.md)\n- [Ext](https://example.com/x.md)\n- [Arch](archive/old.md)\n\n## Something else\n\n- [Not us](c.md)\n";
        let links = parse_active_links(text);
        assert!(links.contains("a.md"), "{links:?}");
        assert!(links.contains("b.md"), "basename kept: {links:?}");
        assert!(
            !links.contains("c.md"),
            "outside the active section: {links:?}"
        );
        assert!(!links.contains("x.md"), "external URL skipped: {links:?}");
        assert!(!links.iter().any(|l| l.contains("archive")), "{links:?}");
    }

    #[test]
    fn active_links_without_a_section_scan_the_whole_text() {
        let links = parse_active_links("- [A](a.md)\n");
        assert!(links.contains("a.md"), "{links:?}");
    }

    // ── generate_future_work_readme ─────────────────────────────────────

    fn doc(path: &str, kind: &str, status: &str) -> Document {
        Document {
            path: path.into(),
            meta: Some(yaml_map(&format!("kind: {kind}\nstatus: {status}\n"))),
            body: String::new(),
        }
    }

    #[test]
    fn future_work_readme_splits_active_from_other() {
        let docs = vec![
            doc("docs/future-work/a-plan.md", "plan", "active"), // docs-check: ignore
            doc("docs/future-work/z-done.md", "plan", "archived"), // docs-check: ignore
            // Not a root future-work leaf — must be skipped entirely.
            doc("docs/other/x.md", "plan", "active"), // docs-check: ignore
        ];
        let readme = generate_future_work_readme(&docs);
        assert!(readme.contains("a-plan.md"), "active: {readme}");
        assert!(readme.contains("z-done.md"), "other: {readme}");
        assert!(
            !readme.contains("docs/other/x.md"), // docs-check: ignore
            "non-future-work must not appear: {readme}"
        );
        assert!(readme.contains("## Active documents"), "{readme}");
    }

    // ── exact_file ──────────────────────────────────────────────────────

    #[test]
    fn exact_file_matches_case_and_leafness() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("a/b")).unwrap();
        fs::write(dir.path().join("a/b/flat.md"), "x").unwrap();
        assert!(exact_file(dir.path(), "a/b/flat.md"));
        assert!(
            exact_file(dir.path(), "a\\b\\flat.md"),
            "backslashes normalised"
        );
        assert!(
            !exact_file(dir.path(), "a/b/FLAT.md"),
            "case must match exactly"
        );
        assert!(!exact_file(dir.path(), "a/b"), "a directory is not a file");
        assert!(!exact_file(dir.path(), "a/b/missing.md"));
        assert!(
            !exact_file(dir.path(), "../a/b/flat.md"),
            "no escaping via .."
        );
    }

    fn yaml_map(text: &str) -> serde_yaml::Mapping {
        let value: serde_yaml::Value = serde_yaml::from_str(text).unwrap();
        value.as_mapping().cloned().unwrap()
    }
}
