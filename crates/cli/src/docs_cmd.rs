//! Documentation maintenance commands.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use regex::Regex;

const DEFAULT_PATHS: &[&str] = &["docs", "README.md", "AGENTS.md", "crates/*/ARCHITECTURE.md"];

/// Check the repository's default Markdown paths for broken relative links.
pub fn check_links() -> Result<(), Box<dyn std::error::Error>> {
    check_links_in(repository_root()?, DEFAULT_PATHS)
}

/// Compiled patterns for one link scan.
struct LinkPatterns {
    link: Regex,
    inline_code: Regex,
    comment: Regex,
    title: Regex,
    external: Regex,
}

impl LinkPatterns {
    fn compile() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(LinkPatterns {
            link: Regex::new(r"\[[^\]\r\n]*\]\(([^\r\n)]+)\)")?,
            inline_code: Regex::new(r"`[^`\r\n]*`")?,
            comment: Regex::new(r"<!--.*?-->")?,
            title: Regex::new(r#"\s+(?:"[^"]*"|'[^']*')\s*$"#)?,
            external: Regex::new(r"^(https?://|//|mailto:|ftp:|tel:|data:|news:|javascript:)")?,
        })
    }
}

/// Normalize one raw link target: strip the optional title, unwrap `<>`, and apply the skip
/// rules. Returns `None` for targets that need no existence check (external, anchors, empty,
/// `.secret`), else the cleaned path (fragment stripped).
fn normalized_link_target(raw: &str, title_re: &Regex, external_re: &Regex) -> Option<String> {
    let mut target = title_re.replace(raw, "").trim().to_string();
    if target.starts_with('<') && target.ends_with('>') {
        target = target[1..target.len() - 1].trim().to_string();
    }
    if target.is_empty()
        || external_re.is_match(&target)
        || target.starts_with('#')
        || target.ends_with(".secret")
    {
        return None;
    }
    let path_target = target
        .split_once('#')
        .map_or(target.as_str(), |(path, _)| path);
    if path_target.trim().is_empty() {
        return None;
    }
    Some(path_target.trim().to_string())
}

/// Scan one markdown file for broken relative links, skipping fenced blocks, inline code, and
/// HTML comments.
fn scan_file(
    root: &Path,
    file: &Path,
    patterns: &LinkPatterns,
    link_count: &mut usize,
    broken: &mut Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = fs::read(file)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut fence: Option<&str> = None;

    for (line_index, line) in text.lines().enumerate() {
        let line_number = line_index + 1;
        if fence.is_some() {
            if is_fence_close(line) {
                fence = None;
            }
            continue;
        }
        if let Some(marker) = fence_marker(line) {
            fence = Some(marker);
            continue;
        }

        let without_code = patterns.inline_code.replace_all(line, "");
        let stripped = patterns.comment.replace_all(&without_code, "");
        for captures in patterns.link.captures_iter(&stripped) {
            let Some(path_target) =
                normalized_link_target(&captures[1], &patterns.title, &patterns.external)
            else {
                continue;
            };
            *link_count += 1;
            let resolved = file
                .parent()
                .expect("a scanned file always has a parent")
                .join(&path_target);
            if !exists_case_insensitively(&resolved) {
                broken.push(format!(
                    "{}:{}: broken link `{}` (resolves to {})",
                    display_path(root, file),
                    line_number,
                    path_target,
                    display_path(root, &resolved)
                ));
            }
        }
    }
    Ok(())
}

/// Print the scan summary and return the pass/fail verdict.
fn report_link_check(
    files: &[PathBuf],
    specs: &[&str],
    link_count: usize,
    broken: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "Docs link check: {} file(s), {} link(s) checked (paths: {})",
        files.len(),
        link_count,
        specs.join(", ")
    );
    if broken.is_empty() {
        println!("PASS: all {link_count} link(s) resolve.");
        return Ok(());
    }
    println!("\nBroken links:");
    for item in broken {
        println!("  {item}");
    }
    Err(format!("{} broken link(s)", broken.len()).into())
}

fn check_links_in(root: PathBuf, specs: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let files = scanned_files(&root, specs)?;
    if files.is_empty() {
        return Err(format!("no markdown files matched: {}", specs.join(", ")).into());
    }

    let patterns = LinkPatterns::compile()?;
    let mut broken = Vec::new();
    let mut link_count = 0usize;

    for file in &files {
        scan_file(&root, file, &patterns, &mut link_count, &mut broken)?;
    }

    report_link_check(&files, specs, link_count, &broken)
}

fn scanned_files(root: &Path, specs: &[&str]) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut files = Vec::new();
    for spec in specs {
        if *spec == "crates/*/ARCHITECTURE.md" {
            for entry in fs::read_dir(root.join("crates"))? {
                add_markdown_file(&mut files, entry?.path().join("ARCHITECTURE.md"));
            }
        } else {
            let path = root.join(spec);
            if path.is_dir() {
                collect_markdown_files(&mut files, &path)?;
            } else if path.is_file() {
                add_markdown_file(&mut files, path);
            }
        }
    }
    files = retain_unignored_files(root, files)?;
    files.sort();
    files.dedup();
    Ok(files)
}

/// Remove files ignored by the repository from generated documentation inputs.
///
/// Local scratch notes can live below `docs/`, including through `.git/info/exclude`. They must
/// not enter generated indexes or link checks because another clone cannot resolve them. Outside a
/// git worktree, such as a temporary docs-site fixture, this keeps the filesystem input unchanged.
pub(crate) fn retain_unignored_files(
    root: &Path,
    files: Vec<PathBuf>,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    if files.is_empty() {
        return Ok(files);
    }

    let mut child = match Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "--stdin", "-z"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return Ok(files),
    };

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or("git check-ignore stdin unavailable")?;
        for file in &files {
            let relative = file
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            stdin.write_all(relative.as_bytes())?;
            stdin.write_all(&[0])?;
        }
    }

    let output = child.wait_with_output()?;
    if !matches!(output.status.code(), Some(0 | 1)) {
        return Ok(files);
    }

    let ignored = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| String::from_utf8_lossy(entry).replace('\\', "/"))
        .collect::<std::collections::HashSet<_>>();

    Ok(files
        .into_iter()
        .filter(|file| {
            file.strip_prefix(root)
                .map(|relative| !ignored.contains(&relative.to_string_lossy().replace('\\', "/")))
                .unwrap_or(true)
        })
        .collect())
}

fn collect_markdown_files(
    files: &mut Vec<PathBuf>,
    directory: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_markdown_files(files, &path)?;
        } else {
            add_markdown_file(files, path);
        }
    }
    Ok(())
}

fn add_markdown_file(files: &mut Vec<PathBuf>, path: PathBuf) {
    if path.is_file()
        && path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
        && !path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".secret"))
    {
        files.push(path);
    }
}

fn exists_case_insensitively(path: &Path) -> bool {
    if path.exists() {
        return true;
    }
    let Some(file_name) = path.file_name() else {
        return false;
    };
    let Some(parent) = path.parent() else {
        return false;
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(&file_name.to_string_lossy())
    })
}

fn fence_marker(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("```") {
        Some("```")
    } else if trimmed.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

fn is_fence_close(line: &str) -> bool {
    matches!(fence_marker(line), Some("```") | Some("~~~")) && line.trim().len() <= 3
}

fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn repository_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let mut candidate = std::env::current_dir()?;
    loop {
        if candidate.join("Cargo.toml").is_file() && candidate.join("crates").is_dir() {
            return Ok(candidate);
        }
        if !candidate.pop() {
            break;
        }
    }
    Err("liberado docs check-links must run inside a Liberado repository".into())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use super::{check_links_in, fence_marker, is_fence_close, retain_unignored_files};

    #[test]
    fn recognizes_fence_boundaries_but_not_fenced_language_text() {
        assert_eq!(fence_marker("```rust"), Some("```"));
        assert_eq!(fence_marker("  ~~~"), Some("~~~"));
        assert!(is_fence_close("```"));
        assert!(!is_fence_close("```rust"));
    }

    #[test]
    fn checks_real_links_and_ignores_examples() {
        let directory = tempfile::tempdir().expect("temp directory");
        let root = directory.path().to_path_buf();
        fs::write(
            root.join("README.md"),
            "[valid](target.md) [external](https://example.com)\n\
             `[inline](missing-inline.md)`\n\
             ```\n             [fenced](missing-fenced.md)\n             ```\n             <!-- [comment](missing-comment.md) -->\n",
        )
        .expect("write README");
        fs::write(root.join("target.md"), "target").expect("write target");

        check_links_in(root, &["README.md"]).expect("only the real link is checked");

        fs::write(directory.path().join("README.md"), "[broken](missing.md)\n")
            .expect("write broken README");
        assert!(check_links_in(directory.path().to_path_buf(), &["README.md"]).is_err());
    }

    #[test]
    fn repository_scans_exclude_ignored_local_notes() {
        let directory = tempfile::tempdir().expect("temp directory");
        let root = directory.path();
        fs::create_dir(root.join("docs")).expect("create docs");
        fs::write(root.join(".gitignore"), "docs/local-note.md\n").expect("write ignore");
        fs::write(root.join("docs/current.md"), "current").expect("write current");
        fs::write(root.join("docs/local-note.md"), "local").expect("write local");
        let init = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root)
            .status()
            .expect("run git init");
        assert!(init.success());

        let files = vec![
            root.join("docs/current.md"),
            root.join("docs/local-note.md"),
        ];
        let retained = retain_unignored_files(root, files).expect("filter ignored files");

        assert_eq!(retained, vec![root.join("docs/current.md")]);
    }
}
