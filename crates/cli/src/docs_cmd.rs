//! Documentation maintenance commands.

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

const DEFAULT_PATHS: &[&str] = &["docs", "README.md", "CLAUDE.md", "crates/*/ARCHITECTURE.md"];

/// Check the repository's default Markdown paths for broken relative links.
pub fn check_links() -> Result<(), Box<dyn std::error::Error>> {
    check_links_in(repository_root()?, DEFAULT_PATHS)
}

fn check_links_in(root: PathBuf, specs: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let files = scanned_files(&root, specs)?;
    if files.is_empty() {
        return Err(format!("no markdown files matched: {}", specs.join(", ")).into());
    }

    let link_re = Regex::new(r"\[[^\]\r\n]*\]\(([^\r\n)]+)\)")?;
    let inline_code_re = Regex::new(r"`[^`\r\n]*`")?;
    let comment_re = Regex::new(r"<!--.*?-->")?;
    let title_re = Regex::new(r#"\s+(?:"[^"]*"|'[^']*')\s*$"#)?;
    let external_re = Regex::new(r"^(https?://|//|mailto:|ftp:|tel:|data:|news:|javascript:)")?;

    let mut broken = Vec::new();
    let mut link_count = 0usize;

    for file in &files {
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

            let without_code = inline_code_re.replace_all(line, "");
            let stripped = comment_re.replace_all(&without_code, "");
            for captures in link_re.captures_iter(&stripped) {
                let mut target = captures[1].to_string();
                target = title_re.replace(&target, "").trim().to_string();
                if target.starts_with('<') && target.ends_with('>') {
                    target = target[1..target.len() - 1].trim().to_string();
                }
                if target.is_empty()
                    || external_re.is_match(&target)
                    || target.starts_with('#')
                    || target.ends_with(".secret")
                {
                    continue;
                }

                let path_target = target
                    .split_once('#')
                    .map_or(target.as_str(), |(path, _)| path);
                if path_target.trim().is_empty() {
                    continue;
                }

                link_count += 1;
                let resolved = file
                    .parent()
                    .expect("a scanned file always has a parent")
                    .join(path_target.trim());
                if !exists_case_insensitively(&resolved) {
                    broken.push(format!(
                        "{}:{}: broken link `{}` (resolves to {})",
                        display_path(&root, file),
                        line_number,
                        path_target.trim(),
                        display_path(&root, &resolved)
                    ));
                }
            }
        }
    }

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
    for item in &broken {
        println!("  {item}");
    }
    Err(format!("{} broken link(s)", broken.len()).into())
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
    files.sort();
    files.dedup();
    Ok(files)
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

    use super::{check_links_in, fence_marker, is_fence_close};

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
}
