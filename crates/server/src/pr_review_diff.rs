//! Exact right-side diff lines used for GitHub inline review comments.

use std::collections::BTreeSet;
use std::path::Path;

use liberado_coder_core::pr_review::MAX_DIFF_BYTES;

pub(crate) fn changed_lines(
    coding_root: &Path,
    base_sha: &str,
    head_sha: &str,
) -> Result<BTreeSet<(String, u32)>, String> {
    let range = format!("{base_sha}...{head_sha}");
    let output = liberado_common::process::std_command("git")
        .args([
            "-c",
            "core.quotePath=false",
            "diff",
            "--unified=0",
            "--no-color",
            "--no-ext-diff",
            &range,
            "--",
        ])
        .current_dir(coding_root)
        .output()
        .map_err(|error| format!("could not read review diff: {error}"))?;
    if !output.status.success() {
        return Err("could not read review diff".into());
    }
    if output.stdout.len() > MAX_DIFF_BYTES {
        return Ok(BTreeSet::new());
    }
    let diff = std::str::from_utf8(&output.stdout).map_err(|_| "review diff was not UTF-8")?;
    Ok(changed_lines_from_diff(diff))
}

fn changed_lines_from_diff(diff: &str) -> BTreeSet<(String, u32)> {
    let mut changed = BTreeSet::new();
    let mut path = None;
    for line in diff.lines() {
        if let Some(raw) = line.strip_prefix("+++ ") {
            path = normalize_new_diff_path(raw);
            continue;
        }
        let Some((start, count)) = line
            .strip_prefix("@@ ")
            .and_then(|header| header.split_whitespace().find(|part| part.starts_with('+')))
            .and_then(parse_new_hunk_range)
        else {
            continue;
        };
        let Some(path) = path.as_ref() else { continue };
        for line_number in start..start.saturating_add(count) {
            changed.insert((path.clone(), line_number));
        }
    }
    changed
}

fn normalize_new_diff_path(raw: &str) -> Option<String> {
    let raw = raw.split('\t').next()?.trim();
    if raw == "/dev/null" {
        return None;
    }
    let decoded = if raw.starts_with('"') {
        serde_json::from_str::<String>(raw).ok()?
    } else {
        raw.to_owned()
    };
    Some(
        decoded
            .strip_prefix("b/")
            .unwrap_or(&decoded)
            .replace('\\', "/"),
    )
}

fn parse_new_hunk_range(raw: &str) -> Option<(u32, u32)> {
    let range = raw.strip_prefix('+')?;
    let (start, count) = range
        .split_once(',')
        .map_or((range, "1"), |(start, count)| (start, count));
    Some((start.parse().ok()?, count.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) -> String {
        let output = liberado_common::process::std_command("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git should start");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git output should be UTF-8")
            .trim()
            .to_owned()
    }

    #[test]
    fn reads_changed_lines_from_a_real_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        git(root, &["init"]);
        git(root, &["config", "user.email", "test@example.invalid"]);
        git(root, &["config", "user.name", "Liberado Test"]);

        std::fs::write(root.join("review.rs"), "one\n").expect("write base");
        git(root, &["add", "review.rs"]);
        git(root, &["commit", "-m", "base"]);
        let base = git(root, &["rev-parse", "HEAD"]);

        std::fs::write(root.join("review.rs"), "one\ntwo\n").expect("write head");
        git(root, &["add", "review.rs"]);
        git(root, &["commit", "-m", "head"]);
        let head = git(root, &["rev-parse", "HEAD"]);

        assert_eq!(
            changed_lines(root, &base, &head).expect("read changed lines"),
            BTreeSet::from([("review.rs".into(), 2)])
        );
    }

    #[test]
    fn tracks_only_right_side_hunk_lines() {
        let diff = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -3,2 +3,3 @@\n-old\n+new\n+more\n context\n@@ -20 +21,0 @@\n-deleted\n";
        assert_eq!(
            changed_lines_from_diff(diff),
            BTreeSet::from([
                ("src/lib.rs".into(), 3),
                ("src/lib.rs".into(), 4),
                ("src/lib.rs".into(), 5),
            ])
        );
    }

    #[test]
    fn handles_new_and_quoted_paths() {
        let diff = "diff --git /dev/null b/new.rs\n--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1,2 @@\n+one\n+two\ndiff --git \"a/a b.rs\" \"b/a b.rs\"\n--- \"a/a b.rs\"\n+++ \"b/a b.rs\"\n@@ -4 +4 @@\n-old\n+new\n";
        let changed = changed_lines_from_diff(diff);
        assert!(changed.contains(&("new.rs".into(), 1)));
        assert!(changed.contains(&("new.rs".into(), 2)));
        assert!(changed.contains(&("a b.rs".into(), 4)));
    }
}
