//! Line-oriented symbol extraction for `list_symbols`.
//!
//! Language is inferred from the file extension. Each extractor looks at one line
//! and returns a declaration name, or `None`. Catalogue assembly and dispatch stay
//! in `lib.rs`; the walk that feeds these extractors stays in `read_search`.

/// Infer a language tag from a path's extension. Empty when the file is not source
/// we know how to scan.
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
#[path = "symbols_tests.rs"]
mod symbols_tests;
