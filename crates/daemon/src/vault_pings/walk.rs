use std::fs;
use std::path::Path;

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".obsidian",
    ".trash",
    "00 - Meta",
    "Legal",
    "proposals",
];

pub(super) fn markdown_notes(root: &Path) -> Vec<(String, String)> {
    let mut notes = Vec::new();
    walk_dir(root, &mut |path, text| {
        notes.push((rel_path(root, path), text.to_string()));
    });
    notes
}

fn walk_dir(dir: &Path, visit: &mut impl FnMut(&Path, &str)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if skip_dir(&name) {
                continue;
            }
            walk_dir(&path, visit);
            continue;
        }
        if !name.ends_with(".md") {
            continue;
        }
        if let Ok(text) = fs::read_to_string(&path) {
            visit(&path, &text);
        }
    }
}

fn skip_dir(name: &str) -> bool {
    name.starts_with('.') || SKIP_DIRS.contains(&name)
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
