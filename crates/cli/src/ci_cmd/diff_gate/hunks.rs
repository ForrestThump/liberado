//! Unified-diff parser for added, modified, deleted, and renamed files.
//!
//! A pure deletion has no `+` line. The parser still records the new-file line where the
//! deletion sits, so the function that lost the line stays in the changed set.

use std::collections::{BTreeMap, BTreeSet};

use super::model::slash;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileChange {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub touched_lines: BTreeSet<u32>,
    pub deleted: bool,
    pub is_rename: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedDiff {
    pub files: Vec<FileChange>,
}

impl ParsedDiff {
    pub fn rename_forward(&self) -> BTreeMap<String, String> {
        rename_map(self, true)
    }

    pub fn rename_reverse(&self) -> BTreeMap<String, String> {
        rename_map(self, false)
    }

    pub fn lines_for(&self, path: &str) -> BTreeSet<u32> {
        let mut lines = BTreeSet::new();
        for file in &self.files {
            if file.new_path.as_deref() == Some(path) {
                lines.extend(&file.touched_lines);
            }
        }
        lines
    }

    /// True when HEAD gained or edited a Rust file. A pure deletion has nothing left to score.
    pub fn touches_rust(&self) -> bool {
        self.files
            .iter()
            .any(|file| rust_path(file.new_path.as_deref()))
    }

    /// Merge-base Rust paths whose functions may be match candidates, including deletions.
    pub fn measured_base_paths(&self) -> BTreeSet<String> {
        self.files
            .iter()
            .filter_map(|file| file.old_path.clone().filter(|path| path.ends_with(".rs")))
            .collect()
    }
}

fn rust_path(path: Option<&str>) -> bool {
    path.is_some_and(|path| path.ends_with(".rs"))
}

fn rename_map(diff: &ParsedDiff, forward: bool) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for file in &diff.files {
        if !file.is_rename {
            continue;
        }
        let (Some(old), Some(new)) = (file.old_path.clone(), file.new_path.clone()) else {
            continue;
        };
        if forward {
            map.insert(old, new);
        } else {
            map.insert(new, old);
        }
    }
    map
}

pub fn parse_unified_diff(text: &str) -> ParsedDiff {
    let mut parser = DiffParser::default();
    for line in text.lines() {
        parser.push(line.trim_end_matches('\r'));
    }
    parser.finish()
}

pub fn touch_entire_file(diff: &mut ParsedDiff, path: &str, line_count: u32) {
    let mut touched = BTreeSet::new();
    for line in 1..=line_count.max(1) {
        touched.insert(line);
    }
    diff.files.push(FileChange {
        old_path: None,
        new_path: Some(slash(path)),
        touched_lines: touched,
        deleted: false,
        is_rename: false,
    });
}

#[derive(Default)]
struct DiffParser {
    files: Vec<FileChange>,
    current: Option<FileChange>,
    in_hunk: bool,
    new_line: u32,
}

impl DiffParser {
    fn push(&mut self, line: &str) {
        if line.starts_with("diff --git ") {
            self.flush();
            self.current = Some(FileChange::default());
            self.in_hunk = false;
            return;
        }
        let Some(file) = self.current.as_mut() else {
            return;
        };
        if self.in_hunk {
            self.push_hunk_line(line);
            return;
        }
        apply_header(file, line);
        if let Some(start) = hunk_new_start(line) {
            self.in_hunk = true;
            self.new_line = start;
        }
    }

    fn push_hunk_line(&mut self, line: &str) {
        if line.starts_with("@@") {
            if let Some(start) = hunk_new_start(line) {
                self.new_line = start;
            }
            return;
        }
        let Some(file) = self.current.as_mut() else {
            return;
        };
        match line.as_bytes().first() {
            Some(b'+') => {
                file.touched_lines.insert(self.new_line);
                self.new_line = self.new_line.saturating_add(1);
            }
            Some(b'-') => {
                file.touched_lines.insert(self.new_line);
            }
            Some(b'\\') => {}
            _ => self.new_line = self.new_line.saturating_add(1),
        }
    }

    fn flush(&mut self) {
        if let Some(file) = self.current.take() {
            self.files.push(file);
        }
        self.in_hunk = false;
    }

    fn finish(mut self) -> ParsedDiff {
        self.flush();
        ParsedDiff { files: self.files }
    }
}

fn apply_header(file: &mut FileChange, line: &str) {
    if let Some(path) = line.strip_prefix("rename from ") {
        file.old_path = Some(slash(path.trim()));
        file.is_rename = true;
    } else if let Some(path) = line.strip_prefix("rename to ") {
        file.new_path = Some(slash(path.trim()));
        file.is_rename = true;
    } else if line.starts_with("deleted file mode") {
        file.deleted = true;
    } else if let Some(path) = header_path(line, "--- ") {
        if file.old_path.is_none() {
            file.old_path = path;
        }
    } else if let Some(path) = header_path(line, "+++ ") {
        let deleted = path.is_none();
        if file.new_path.is_none() {
            file.new_path = path;
        }
        if deleted {
            file.deleted = true;
        }
    }
}

fn header_path(line: &str, marker: &str) -> Option<Option<String>> {
    let rest = line.strip_prefix(marker)?.trim();
    if rest == "/dev/null" {
        return Some(None);
    }
    let path = rest
        .strip_prefix("a/")
        .or_else(|| rest.strip_prefix("b/"))
        .unwrap_or(rest);
    Some(Some(slash(path)))
}

fn hunk_new_start(line: &str) -> Option<u32> {
    let plus = line.find(" +")?;
    if !line.starts_with("@@") {
        return None;
    }
    let coord = line[plus + 2..].split_whitespace().next()?;
    let start = coord.split(',').next()?;
    start.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{parse_unified_diff, touch_entire_file};

    #[test]
    fn parses_added_modified_deleted_renamed_and_crlf() {
        let diff = "\
diff --git a/old.rs b/new.rs\r
similarity index 90%\r
rename from old.rs\r
rename to new.rs\r
--- a/old.rs\r
+++ b/new.rs\r
@@ -2,0 +3 @@\r
+    let added = 1;\r
@@ -8 +8,0 @@\r
-    let gone = 2;\r
diff --git a/fresh.rs b/fresh.rs\r
new file mode 100644\r
--- /dev/null\r
+++ b/fresh.rs\r
@@ -0,0 +1,2 @@\r
+fn fresh() {}\r
+fn next() {}\r
diff --git a/gone.rs b/gone.rs\r
deleted file mode 100644\r
--- a/gone.rs\r
+++ /dev/null\r
@@ -1 +0,0 @@\r
-fn gone() {}\r
";
        let parsed = parse_unified_diff(diff);
        assert_eq!(parsed.files.len(), 3);

        let renamed = &parsed.files[0];
        assert!(renamed.is_rename);
        assert_eq!(renamed.old_path.as_deref(), Some("old.rs"));
        assert_eq!(renamed.new_path.as_deref(), Some("new.rs"));
        assert!(renamed.touched_lines.contains(&3));
        assert!(renamed.touched_lines.contains(&8));
        assert_eq!(
            parsed.rename_forward().get("old.rs").map(String::as_str),
            Some("new.rs")
        );

        let fresh = &parsed.files[1];
        assert_eq!(
            fresh.touched_lines.iter().copied().collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(fresh.old_path.is_none());

        let gone = &parsed.files[2];
        assert!(gone.deleted);
        assert_eq!(gone.old_path.as_deref(), Some("gone.rs"));
        assert!(gone.new_path.is_none());
    }

    #[test]
    fn an_untracked_file_counts_as_every_line_added() {
        let mut parsed = parse_unified_diff("");
        touch_entire_file(&mut parsed, "src\\new.rs", 2);
        assert_eq!(
            parsed
                .lines_for("src/new.rs")
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }
}
