//! Read, search, and list tooling for the coding pack.
//!
//! `list_files`, `grep` (and the `search_text` alias), `list_symbols`, and `read_file`,
//! plus the helpers those handlers need. Catalogue assembly and dispatch stay in `lib.rs`.

use std::{collections::VecDeque, path::Path};

use liberado_coder_core::PathPolicy;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{CodingToolRuntime, ToolError, fs_err, parse_args, path_denied};

impl CodingToolRuntime {
    pub(crate) async fn list_files(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            #[serde(default = "default_limit")]
            limit: usize,
        }
        let args: Args = parse_args(args)?;
        let mut files = Vec::new();
        walk_files(
            self.workspace.root(),
            args.limit,
            &self.path_policy,
            |_, rel| {
                files.push(rel.to_string());
                true
            },
        )?;
        Ok(json!({ "files": files, "limit": args.limit }))
    }

    /// Search file contents by regular expression.
    ///
    /// Named `grep` because that is the word the model already knows. The predecessor was called
    /// `search_text`, described in six words as "Search workspace files for exact text", and did
    /// literal `line.contains` only. An A/B against Kilo Code on the same task, same model and
    /// same repo showed what that cost: Kilo's run called `grep` **17 times** and `read` 22 times
    /// against **6** edits — 6.5 reads per edit — and produced code that compiled. Ours called
    /// `search_text` **once**, at 1.0 reads per edit, and produced code that did not.
    ///
    /// A model that cannot cheaply ask "where is this used" edits from memory instead, which is
    /// how a run invents `self.observers` for a field that is singular.
    pub(crate) async fn grep(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            /// `query` is the predecessor's name for this. Accepted so a run that started against
            /// the old catalog does not fail on a rename it never saw — a half-alias that keeps
            /// the tool name and drops its parameters would be worse than a clean break.
            #[serde(alias = "query")]
            pattern: String,
            /// Subdirectory to search, relative to the workspace root.
            #[serde(default)]
            path: Option<String>,
            /// Basename glob, e.g. `*.rs`. Matched against the file name, not the whole path —
            /// a path-anchored pattern is the classic way to silently match nothing.
            #[serde(default)]
            glob: Option<String>,
            /// `content` | `files_with_matches` | `count`.
            #[serde(default = "default_output_mode")]
            output_mode: String,
            /// Case-insensitive.
            #[serde(default, rename = "-i")]
            case_insensitive: bool,
            /// Lines of context either side of a match. `content` mode only.
            #[serde(default, rename = "-C")]
            context: usize,
            #[serde(default = "default_head_limit", alias = "limit")]
            head_limit: usize,
            /// Treat `pattern` as literal text rather than a regex.
            #[serde(default)]
            fixed_strings: bool,
        }

        let args: Args = parse_args(args)?;
        if args.pattern.is_empty() {
            return Err(ToolError::BadRequest(
                "pattern must not be empty".to_string(),
            ));
        }

        let pattern = if args.fixed_strings {
            regex::escape(&args.pattern)
        } else {
            args.pattern.clone()
        };
        let re = regex::RegexBuilder::new(&pattern)
            .case_insensitive(args.case_insensitive)
            .build()
            .map_err(|e| {
                // A rejected pattern must say what to do next. Ripgrep regex is not POSIX, and a
                // model that gets only "invalid regex" retries the same thing with more escapes.
                ToolError::BadRequest(format!(
                    "pattern is not a valid regex: {e}. Braces, parentheses and brackets are \
                     special — escape them, or pass \"fixed_strings\": true to search for the \
                     text literally."
                ))
            })?;

        let search_root = match &args.path {
            Some(p) => self.rel_path(p, false)?,
            None => self.workspace.root().to_path_buf(),
        };

        let mut hits: Vec<Value> = Vec::new();
        let mut files_with_matches: Vec<String> = Vec::new();
        let mut counts: Vec<Value> = Vec::new();
        let mut total = 0usize;
        // Kept for the fuzzy fallback below: scanning twice would double the walk.
        let mut sampled: Vec<(String, usize, String)> = Vec::new();

        walk_files(
            &search_root,
            self.path_policy.search_max_results,
            &self.path_policy,
            |path, rel| {
                if let Some(glob) = &args.glob
                    && !glob_matches_basename(glob, rel)
                {
                    return true;
                }
                let Ok(content) = std::fs::read_to_string(path) else {
                    return true;
                };
                let lines: Vec<&str> = content.lines().collect();
                let mut file_count = 0usize;
                for (idx, line) in lines.iter().enumerate() {
                    if sampled.len() < FUZZY_SAMPLE_LINES {
                        sampled.push((rel.to_string(), idx + 1, (*line).to_string()));
                    }
                    if !re.is_match(line) {
                        continue;
                    }
                    file_count += 1;
                    total += 1;
                    if args.output_mode == "content" && hits.len() < args.head_limit {
                        let lo = idx.saturating_sub(args.context);
                        let hi = (idx + args.context + 1).min(lines.len());
                        hits.push(json!({
                            "path": rel,
                            "line": idx + 1,
                            "text": line,
                            "context": if args.context > 0 {
                                Value::String(lines[lo..hi].join("\n"))
                            } else { Value::Null },
                        }));
                    }
                }
                if file_count > 0 {
                    if files_with_matches.len() < args.head_limit {
                        files_with_matches.push(rel.to_string());
                    }
                    if counts.len() < args.head_limit {
                        counts.push(json!({ "path": rel, "count": file_count }));
                    }
                }
                true
            },
        )?;

        // Nothing matched: say what was nearly it rather than returning an empty list.
        //
        // An empty result is the least useful answer a search can give — the model cannot tell a
        // wrong pattern from an absent symbol, and both runs that failed on invented anchors were
        // working from exactly that ambiguity. The near misses reuse the same similarity metric
        // the edit tool matches anchors with, so "close" means the same thing in both places.
        if total == 0 {
            // Score on identifiers, not whole lines.
            //
            // The first version compared the pattern against each line and found nothing useful:
            // the anchor a real run invented, `for observer in &self.observers`, is only ~35%
            // similar to the line that actually exists, `let Some(observer) = self.observer...`.
            // The signal a reader wants is in the *token* — `observers` against `observer` is
            // 89% — because the mistake is almost always a name, not a sentence.
            let wanted: Vec<String> = identifiers(&args.pattern);
            let mut near: Vec<(f64, &(String, usize, String))> = sampled
                .iter()
                .filter_map(|s| {
                    let best = identifiers(&s.2)
                        .iter()
                        .flat_map(|tok| {
                            wanted
                                .iter()
                                .map(move |w| crate::fuzzy_match::similarity(w, tok))
                        })
                        .fold(0.0f64, f64::max);
                    (best >= FUZZY_SUGGEST_THRESHOLD).then_some((best, s))
                })
                .collect();
            near.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            near.dedup_by(|a, b| a.1.2 == b.1.2);
            let did_you_mean: Vec<Value> = near
                .iter()
                .take(5)
                .map(|(score, s)| {
                    json!({ "path": s.0, "line": s.1, "text": s.2.trim(), "similarity": score })
                })
                .collect();
            return Ok(json!({
                "pattern": args.pattern,
                "total": 0,
                "matches": [],
                "did_you_mean": did_you_mean,
            }));
        }

        Ok(match args.output_mode.as_str() {
            "content" => json!({ "pattern": args.pattern, "total": total, "matches": hits }),
            "count" => json!({ "pattern": args.pattern, "total": total, "counts": counts }),
            _ => json!({
                "pattern": args.pattern,
                "total": total,
                "files": files_with_matches,
            }),
        })
    }

    pub(crate) async fn list_symbols(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            #[serde(default = "default_limit")]
            limit: usize,
        }
        let args: Args = parse_args(args)?;
        let mut files = Vec::new();
        // `limit` bounds files *returned*; `SYMBOL_SCAN_MAX_FILES` bounds files looked at. A tree
        // is mostly not source, so spending the same budget on both is what made this return one
        // file — a stray `validate.py` — against this repo: the 200 slots went to markdown and
        // manifests before the walk reached `crates/`.
        walk_files(
            self.workspace.root(),
            SYMBOL_SCAN_MAX_FILES,
            &self.path_policy,
            |path, rel| {
                if files.len() >= args.limit {
                    return false;
                }
                // Decide from the extension before touching the file. Reading first loads every
                // asset, lockfile and binary in the tree whole and then discards it — and
                // `read_to_string` only fails on a binary *after* it has read all of it.
                if lang_from_path(rel).is_empty() {
                    return true;
                }
                let Ok(bytes) = std::fs::read(path) else {
                    return true;
                };
                let capped = cap_bytes(bytes, self.path_policy.read_max_bytes);
                let content = String::from_utf8_lossy(&capped);
                let symbols = extract_symbols(rel, &content);
                if !symbols.is_empty() {
                    files.push(json!({
                        "path": rel,
                        "symbols": symbols,
                    }));
                }
                true
            },
        )?;
        Ok(json!({ "files": files, "limit": args.limit }))
    }

    pub(crate) async fn read_file(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            path: String,
            #[serde(default)]
            start_line: Option<usize>,
            #[serde(default)]
            line_count: Option<usize>,
        }
        let args: Args = parse_args(args)?;
        let path = self.rel_path(&args.path, false)?;
        let bytes = std::fs::read(&path).map_err(fs_err)?;
        let capped = cap_bytes(bytes, self.path_policy.read_max_bytes);
        let full = String::from_utf8_lossy(&capped).into_owned();
        let start = args.start_line.unwrap_or(1).max(1);
        let content = slice_lines(&full, args.start_line, args.line_count);

        if self.hashline.enabled {
            // Hash is always over the full file so tags stay stable across partial reads.
            let tag = crate::hashline::compute_file_hash(&full, self.hashline.hash_length);
            let mut formatted = crate::hashline::format_header(&args.path, &tag);
            formatted.push('\n');
            for (i, line) in content.split('\n').enumerate() {
                let line = line.strip_suffix('\r').unwrap_or(line);
                if i > 0 {
                    formatted.push('\n');
                }
                formatted.push_str(&format!("{}:{line}", start + i));
            }
            if content.ends_with('\n') {
                formatted.push('\n');
            }
            return Ok(json!({
                "path": args.path,
                "content": formatted,
                "hashline": true,
                "tag": tag,
                "start_line": start,
            }));
        }

        Ok(json!({ "path": args.path, "content": content }))
    }
}

pub(crate) fn default_limit() -> usize {
    200
}

/// How many files `list_symbols` will look at while trying to fill its result limit.
///
/// The walk still has to terminate on a repository far larger than any limit could describe, but
/// this is a scan bound rather than a result bound — the two are not the same number.
const SYMBOL_SCAN_MAX_FILES: usize = 20_000;

pub(crate) fn cap_bytes(mut bytes: Vec<u8>, max: usize) -> Vec<u8> {
    if bytes.len() > max {
        bytes.truncate(max);
    }
    bytes
}

pub(crate) fn slice_lines(
    content: &str,
    start_line: Option<usize>,
    line_count: Option<usize>,
) -> String {
    let start = start_line.unwrap_or(1).saturating_sub(1);
    let count = line_count.unwrap_or(usize::MAX);
    content
        .lines()
        .skip(start)
        .take(count)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Walk `root` breadth-first, visiting up to `limit` files the policy does not deny.
///
/// The deny list prunes **directories** as well as files, and a denied entry does not spend a
/// slot. Filtering at the visitor instead — after the walk has already counted the file — means
/// the budget goes to paths the caller was never allowed to read. `PathPolicy::default()` denies
/// `.git/**`, `target/**` and `node_modules/**`, which between them are almost every file in a
/// working checkout: run against this repo, `list_symbols` at the default limit of 200 came back
/// with one file, and it was a stray Python script.
/// `visit` returns whether to keep walking — `false` stops immediately. A caller whose limit is on
/// what it *collects* rather than what it *reads* needs that: `list_symbols` has to look past the
/// markdown and lockfiles to find any source at all, so its `limit` cannot also be the walk budget.
/// Lines sampled while walking, for the "did you mean" fallback when nothing matched.
///
/// Bounded because the fallback is a courtesy, not a search: holding a whole tree in memory to
/// answer a query that found nothing would be a poor trade.
const FUZZY_SAMPLE_LINES: usize = 4000;

/// How close an identifier must be to be worth suggesting. Lower than the edit tool's 0.95
/// because the costs are not comparable: there a wrong answer edits the wrong place, here it is
/// one suggestion the reader ignores.
const FUZZY_SUGGEST_THRESHOLD: f64 = 0.7;

/// Identifier-ish tokens, long enough to be worth comparing.
///
/// Four characters filters out `let`, `fn`, `in` and friends, which otherwise match everything
/// and drown the real suggestion.
pub(crate) fn identifiers(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() >= 4)
        .map(str::to_lowercase)
        .collect()
}

pub(crate) fn default_output_mode() -> String {
    "files_with_matches".to_string()
}

pub(crate) fn default_head_limit() -> usize {
    250
}

/// Match a glob against a path's **basename** only.
///
/// Anchored patterns like `src/**/*.rs` are the classic way to match nothing by accident; the
/// tool description says to scope with `path` instead, and this keeps the behaviour honest rather
/// than half-supporting a syntax that would surprise.
pub(crate) fn glob_matches_basename(glob: &str, rel: &str) -> bool {
    let name = rel.rsplit(['/', '\\']).next().unwrap_or(rel);
    glob_match(glob, name)
}

/// `*` and `?` only. A full glob engine is a dependency we do not need for filename filtering.
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

pub(crate) fn walk_files(
    root: &Path,
    limit: usize,
    policy: &PathPolicy,
    mut visit: impl FnMut(&Path, &str) -> bool,
) -> Result<(), ToolError> {
    // A file path is a valid grep/list target. `read_dir` on a file is
    // "The directory name is invalid. (os error 267)" on Windows.
    if root.is_file() {
        let rel = root
            .file_name()
            .map(|n| n.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let _ = visit(root, &rel);
        return Ok(());
    }
    if !root.is_dir() {
        return Err(ToolError::BadRequest(format!(
            "path is not a searchable file or directory: {}",
            root.display()
        )));
    }
    let mut queue = VecDeque::from([root.to_path_buf()]);
    let mut visited = 0usize;
    while let Some(dir) = queue.pop_front() {
        for entry in std::fs::read_dir(&dir).map_err(fs_err)? {
            let entry = entry.map_err(fs_err)?;
            let path = entry.path();
            let rel = relative_string(root, &path);
            if path_denied(&rel, policy) {
                continue;
            }
            if path.is_dir() {
                queue.push_back(path);
            } else if path.is_file() {
                if !visit(&path, &rel) {
                    return Ok(());
                }
                visited += 1;
                if visited >= limit {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn relative_string(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub(crate) fn lang_from_path(path: &str) -> &str {
    let lower = path.to_lowercase();
    if lower.ends_with(".rs") {
        "rust"
    } else if lower.ends_with(".py") {
        "python"
    } else if lower.ends_with(".ts")
        || lower.ends_with(".tsx")
        || lower.ends_with(".js")
        || lower.ends_with(".jsx")
    {
        "typescript"
    } else if lower.ends_with(".go") {
        "go"
    } else if lower.ends_with(".java") {
        "java"
    } else {
        ""
    }
}

pub(crate) fn extract_symbols(path: &str, content: &str) -> Vec<String> {
    let lang = lang_from_path(path);
    if lang.is_empty() {
        return Vec::new();
    }
    let mut symbols = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        let sym = match lang {
            "rust" => extract_rust_symbol(trimmed),
            // Python alone gets the raw line: indentation *is* its nesting, so a trimmed line
            // cannot tell a module-level `def` from a method. Passing `trimmed` here made the
            // column-0 guard in `extract_python_symbol` a check that could not fire.
            "python" => extract_python_symbol(line),
            "typescript" => extract_ts_symbol(trimmed),
            "go" => extract_go_symbol(trimmed),
            "java" => extract_java_symbol(trimmed),
            _ => None,
        };
        if let Some(s) = sym {
            if symbols.len() >= 50 {
                break;
            }
            symbols.push(s);
        }
    }
    symbols
}

/// `keyword` -> the characters that terminate its declaration name.
const RUST_DECL_TERMINATORS: &[(&str, &[char])] = &[
    ("fn", &['(', '<']),
    ("struct", &['<', '{', '(', ';']),
    ("enum", &['<', '{']),
    ("trait", &['<', '{']),
    ("mod", &['{', ';']),
];

/// True when the trimmed line is a comment or a block-comment continuation. Shared by the
/// C-family symbol extractors (`//`, `/*`, and `*` continuation lines).
pub(crate) fn is_comment_line(line: &str) -> bool {
    line.starts_with("//") || line.starts_with("/*") || line.starts_with('*')
}

/// Strip each of `prefixes` in order from the front of `s`.
pub(crate) fn strip_prefixes<'a>(s: &'a str, prefixes: &[&str]) -> &'a str {
    let mut rest = s;
    for prefix in prefixes {
        rest = rest.trim_start_matches(prefix);
    }
    rest
}

/// The identifier following `prefix` on a declaration line, cut at the first structural
/// terminator. `None` when the line does not start with `prefix` or the name is empty.
///
/// Shared by every language extractor — Rust, Python, TypeScript, Go and Java all produce
/// symbol names by trimming a keyword prefix and cutting at structural punctuation.
pub(crate) fn symbol_after(trimmed: &str, prefix: &str, terminators: &[char]) -> Option<String> {
    if !trimmed.starts_with(prefix) {
        return None;
    }
    let name = trimmed
        .trim_start_matches(prefix)
        .split(terminators)
        .next()?
        .trim()
        .to_string();
    (!name.is_empty()).then_some(name)
}

/// Strip a same-line `#[...]` attribute, leaving whatever declaration follows it (if any).
pub(crate) fn strip_same_line_attribute(line: &str) -> &str {
    if line.starts_with("#[") {
        line.split(']').nth(1).unwrap_or("").trim()
    } else {
        line
    }
}

/// Strip visibility and qualifier prefixes, leaving just the keyword prefix.
/// Handles: pub, pub(crate), pub(super), pub(in path), const, unsafe, async, extern "C".
pub(crate) fn strip_rust_prefixes(line: &str) -> &str {
    line.trim_start_matches("pub(crate) ")
        .trim_start_matches("pub(super) ")
        .trim_start_matches("pub ")
        .trim_start_matches("const ")
        .trim_start_matches("unsafe ")
        .trim_start_matches("async ")
        .trim_start_matches("extern \"C\" ")
}

/// Extract `keyword <name>` from a trimmed declaration line by delegating to the shared
/// name extraction.
pub(crate) fn rust_keyword_symbol(
    rest: &str,
    keyword: &str,
    terminators: &[char],
) -> Option<String> {
    let name = symbol_after(rest, &format!("{keyword} "), terminators)?;
    Some(format!("{keyword} {name}"))
}

/// Extract `impl <name>` from a declaration that may carry generic parameters.
pub(crate) fn extract_impl_symbol(rest: &str) -> Option<String> {
    let rest = rest.trim_start_matches("impl").trim_start_matches(' ');
    let name = if rest.starts_with('<') {
        rest.split('>')
            .nth(1)
            .and_then(|s| s.trim().split(' ').next())?
            .to_string()
    } else {
        rest.split(['<', '{', ' ']).next()?.trim().to_string()
    };
    if !name.is_empty() && !name.starts_with("for") {
        Some(format!("impl {name}"))
    } else {
        None
    }
}

pub(crate) fn extract_rust_symbol(line: &str) -> Option<String> {
    let line = line.trim();
    if is_comment_line(line) {
        return None;
    }
    let line = strip_same_line_attribute(line);
    if line.is_empty() {
        return None;
    }
    let rest = strip_rust_prefixes(line);
    // `impl` has its own generics/for handling; every other declaration keyword shares one
    // name-extraction shape (keyword -> structural terminators), so it is table-driven.
    if rest.starts_with("impl<") || rest.starts_with("impl ") {
        return extract_impl_symbol(rest);
    }
    for (keyword, terminators) in RUST_DECL_TERMINATORS {
        if let Some(sym) = rust_keyword_symbol(rest, keyword, terminators) {
            return Some(sym);
        }
    }
    None
}
pub(crate) fn extract_python_symbol(line: &str) -> Option<String> {
    // Indentation is Python's nesting. A trimmed line cannot tell a module-level `def` from a
    // method, so the caller hands us the raw line and we reject anything indented.
    if line.starts_with(' ') || line.starts_with('\t') {
        return None;
    }
    let trimmed = line.trim();
    if trimmed.starts_with('#') {
        return None;
    }
    if let Some(name) = symbol_after(trimmed, "def ", &['('])
        && !name.starts_with('_')
    {
        return Some(format!("def {name}"));
    }
    if let Some(name) = symbol_after(trimmed, "class ", &['(', ':']) {
        return Some(format!("class {name}"));
    }
    None
}

pub(crate) fn extract_ts_symbol(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if is_comment_line(trimmed) {
        return None;
    }
    if let Some(s) = ts_plain_function(trimmed) {
        return Some(s);
    }
    if let Some(s) = ts_export_function(trimmed) {
        return Some(s);
    }
    if let Some(s) = ts_class(trimmed) {
        return Some(s);
    }
    if let Some(s) = ts_interface(trimmed) {
        return Some(s);
    }
    ts_const(trimmed)
}

/// A plain `function name(` at line start.
pub(crate) fn ts_plain_function(trimmed: &str) -> Option<String> {
    let name = symbol_after(trimmed, "function ", &['('])?;
    Some(format!("function {name}"))
}

/// An exported function, with or without `default` / `async` qualifiers.
pub(crate) fn ts_export_function(trimmed: &str) -> Option<String> {
    if !(trimmed.starts_with("export function ")
        || trimmed.starts_with("export default function ")
        || trimmed.starts_with("export default async function "))
    {
        return None;
    }
    let rest = strip_prefixes(
        trimmed,
        &["export default async ", "export default ", "export "],
    );
    let name = symbol_after(rest, "function ", &['('])?;
    Some(format!("export function {name}"))
}

/// A `class Name` (optionally exported / default / abstract).
pub(crate) fn ts_class(trimmed: &str) -> Option<String> {
    if !(trimmed.contains(" class ") || trimmed.starts_with("class ")) {
        return None;
    }
    let rest = strip_prefixes(
        trimmed,
        if trimmed.starts_with("export ") {
            &["export ", "default ", "abstract "]
        } else {
            &["abstract "]
        },
    );
    let name = symbol_after(rest, "class ", &['<', '{', ' ', ':'])?;
    if name != "extends" && name != "implements" {
        return Some(format!("class {name}"));
    }
    None
}

/// An `interface` declaration (optionally exported).
pub(crate) fn ts_interface(trimmed: &str) -> Option<String> {
    if trimmed.starts_with("interface ") || trimmed.starts_with("export interface ") {
        let rest = strip_prefixes(trimmed, &["export "]);
        let name = symbol_after(rest, "interface ", &['<', '{'])?;
        return Some(format!("interface {name}"));
    }
    None
}

/// A `const` arrow binding.
pub(crate) fn ts_const(trimmed: &str) -> Option<String> {
    if trimmed.starts_with("export const ") || trimmed.starts_with("const ") {
        let rest = strip_prefixes(trimmed, &["export "]);
        let name = symbol_after(rest, "const ", &[':', '='])?;
        if rest.contains("=>") {
            return Some(format!("const {name}"));
        }
    }
    None
}

pub(crate) fn extract_go_symbol(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if is_comment_line(trimmed) {
        return None;
    }
    if let Some(name) = symbol_after(trimmed, "func ", &['(']) {
        return Some(format!("func {name}"));
    }
    if trimmed.starts_with("type ") {
        let name = trimmed
            .trim_start_matches("type ")
            .split_whitespace()
            .next()?
            .to_string();
        if trimmed.contains(" struct") {
            return Some(format!("type {name} struct"));
        }
        if trimmed.contains(" interface") {
            return Some(format!("type {name} interface"));
        }
    }
    None
}

pub(crate) fn extract_java_symbol(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if is_comment_line(trimmed) {
        return None;
    }
    let rest = strip_prefixes(trimmed, &["public "]);
    if let Some(name) = symbol_after(rest, "class ", &['<', '{']) {
        return Some(format!("class {name}"));
    }
    if let Some(name) = symbol_after(rest, "interface ", &['<', '{']) {
        return Some(format!("interface {name}"));
    }
    if let Some(name) = symbol_after(rest, "enum ", &['<', '{']) {
        return Some(format!("enum {name}"));
    }
    None
}

#[cfg(test)]
#[path = "read_search_tests.rs"]
mod read_search_tests;
