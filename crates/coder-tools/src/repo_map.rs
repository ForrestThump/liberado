//! Repository map — symbol extraction + dependency-graph ranking for
//! cold-start repo awareness.  Based on Aider's repo-map algorithm.
//!
//! # Algorithm
//! 1. Walk the workspace, detect source files by extension, parse each with
//!    tree-sitter to extract *definition* and *reference* tags.
//! 2. Build a weighted dependency graph: nodes = files, edges = reference →
//!    definition (weighted by sqrt(reference-count)).
//! 3. Run PageRank with a personalisation vector to rank files by importance.
//! 4. Distribute file ranks across the definitions they contain, sort by rank,
//!    and trim to a token budget.
//! 5. Render as a compact text tree the LLM can read in one shot.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

// ── data structures ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Tag {
    file: String,
    name: String,
    is_def: bool,
    line: usize,
    snippet: String,
}

#[derive(Debug, Clone)]
struct RankedDef {
    file: String,
    name: String,
    rank: f64,
    line: usize,
    snippet: String,
}

// ── language support ───────────────────────────────────────────────────────

const SOURCE_EXTENSIONS: &[&str] = &["rs", "py", "pyi", "ts", "tsx", "js", "jsx", "go"];

const MAX_FILES: usize = 500;
const MAX_SCAN_FILES: usize = 5_000;
const MAX_FILE_SIZE: usize = 200_000;

/// Extract stable search terms from a coding task for opt-in context routing.
///
/// Code-like names and paths survive intact (`ResourceLink`, `foo::bar`, `src/lib.rs`). Common
/// prose words are removed so a long task cannot make every source file look relevant.
pub fn extract_task_terms(task: &str) -> Vec<String> {
    const STOP_WORDS: &[&str] = &[
        "about",
        "acceptance",
        "after",
        "before",
        "change",
        "correct",
        "ensure",
        "from",
        "have",
        "implement",
        "into",
        "must",
        "should",
        "tests",
        "that",
        "their",
        "then",
        "these",
        "this",
        "when",
        "where",
        "which",
        "with",
        "without",
    ];

    let mut seen = HashSet::new();
    let mut terms: Vec<(usize, u8, String)> = task
        .split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '\\' | ':')))
        .map(|term| term.trim_matches(['.', '/', '\\', ':', '-']))
        .filter(|term| term.len() >= 3)
        .filter(|term| {
            let lower = term.to_ascii_lowercase();
            !STOP_WORDS.contains(&lower.as_str()) && lower.chars().any(|c| c.is_alphabetic())
        })
        .enumerate()
        .filter(|(_, term)| seen.insert(term.to_ascii_lowercase()))
        .map(|(position, term)| {
            let code_like = term.contains(['_', '-', '.', '/', '\\', ':'])
                || term.chars().skip(1).any(|c| c.is_uppercase())
                || (term.len() <= 8 && term.chars().all(|c| !c.is_lowercase()));
            (position, u8::from(code_like), term.to_string())
        })
        .collect();
    terms.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    terms
        .into_iter()
        .map(|(_, _, term)| term)
        .take(32)
        .collect()
}

fn detect_lang(path: &str) -> Option<(&'static str, Language)> {
    let ext = path.rsplit('.').next()?;
    match ext {
        "rs" => Some(("rust", tree_sitter_rust::LANGUAGE.into())),
        "py" | "pyi" => Some(("python", tree_sitter_python::LANGUAGE.into())),
        "ts" => Some((
            "typescript",
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        )),
        "tsx" => Some(("tsx", tree_sitter_typescript::LANGUAGE_TSX.into())),
        "js" | "jsx" => Some(("tsx", tree_sitter_typescript::LANGUAGE_TSX.into())),
        "go" => Some(("go", tree_sitter_go::LANGUAGE.into())),
        _ => None,
    }
}

// ── tree-sitter query sources (cached as strings) ──────────────────────────

fn query_source(lang_name: &str) -> &'static str {
    match lang_name {
        "rust" => {
            r#"
(function_item name: (identifier) @def.func)
(struct_item name: (type_identifier) @def.type)
(enum_item name: (type_identifier) @def.type)
(union_item name: (type_identifier) @def.type)
(type_item name: (type_identifier) @def.type)
(trait_item name: (type_identifier) @def.trait)
(macro_definition name: (identifier) @def.macro)
(const_item name: (identifier) @def.const)
(mod_item name: (identifier) @def.module)
(static_item name: (identifier) @def.const)
(call_expression function: (identifier) @ref.call)
(call_expression function: (field_expression field: (field_identifier) @ref.call))
(macro_invocation macro: (identifier) @ref.call)
"#
        }
        "python" => {
            r#"
(class_definition name: (identifier) @def.type)
(function_definition name: (identifier) @def.func)
(call function: (identifier) @ref.call)
(call function: (attribute attribute: (identifier) @ref.call))
"#
        }
        "typescript" | "tsx" => {
            r#"
(function_declaration name: (identifier) @def.func)
(method_definition name: (property_identifier) @def.func)
(class_declaration name: (type_identifier) @def.type)
(interface_declaration name: (type_identifier) @def.type)
(type_alias_declaration name: (type_identifier) @def.type)
(enum_declaration name: (identifier) @def.type)
(lexical_declaration
  (variable_declarator
    name: (identifier) @def.func
    value: [(arrow_function) (function_expression)]))
(export_statement
  declaration: (lexical_declaration
    (variable_declarator
      name: (identifier) @def.func
      value: [(arrow_function) (function_expression)])))
(call_expression function: (identifier) @ref.call)
(call_expression function: (member_expression property: (property_identifier) @ref.call))
(new_expression constructor: (identifier) @ref.call)
"#
        }
        "go" => {
            r#"
(function_declaration name: (identifier) @def.func)
(method_declaration name: (field_identifier) @def.func)
(type_spec name: (type_identifier) @def.type)
(call_expression function: (identifier) @ref.call)
(call_expression function: (selector_expression field: (field_identifier) @ref.call))
"#
        }
        _ => "",
    }
}

// ── tag extraction ─────────────────────────────────────────────────────────

fn extract_tags(file_path: &str, source: &str, lang_name: &str, lang: &Language) -> Vec<Tag> {
    let mut parser = Parser::new();
    if parser.set_language(lang).is_err() {
        return Vec::new();
    }
    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let query_src = query_source(lang_name);
    if query_src.is_empty() {
        return Vec::new();
    }
    let Ok(query) = Query::new(lang, query_src) else {
        return Vec::new();
    };

    let mut cursor = QueryCursor::new();
    let source_bytes = source.as_bytes();
    let root = tree.root_node();

    let mut seen: HashSet<(String, bool, usize)> = HashSet::new();
    let mut tags: Vec<Tag> = Vec::new();

    let mut captures = cursor.captures(&query, root, source_bytes);
    while let Some((m, capture_index)) = captures.next() {
        let capture = &m.captures[*capture_index];
        let capture_name = query.capture_names()[capture.index as usize];
        let (is_def, _kind) = match capture_name.split_once('.') {
            Some(("def", kind)) => (true, kind),
            Some(("ref", kind)) => (false, kind),
            _ => continue,
        };
        let node = capture.node;
        let Ok(name) = node.utf8_text(source_bytes) else {
            continue;
        };
        let name_str: &str = name;
        if name_str.len() < 2 || name_str.len() > 80 {
            continue;
        }
        let name = name_str.to_string();
        let line = node.start_position().row + 1;

        if !seen.insert((name.clone(), is_def, line)) {
            continue;
        }

        let line_idx = node.start_position().row;
        let snippet = source
            .lines()
            .nth(line_idx)
            .map(|l| l.trim().chars().take(120).collect::<String>())
            .unwrap_or_default();

        tags.push(Tag {
            file: file_path.to_string(),
            name,
            is_def,
            line,
            snippet,
        });
    }

    tags
}

// ── graph building ─────────────────────────────────────────────────────────

struct DepGraph {
    file_count: usize,
    file_names: Vec<String>,
    edges: Vec<(usize, usize, f64)>,
    file_tags: Vec<Vec<Tag>>,
}

fn build_dep_graph(all_tags: &[Tag]) -> DepGraph {
    let mut file_set: Vec<String> = Vec::new();
    let mut file_idx: HashMap<String, usize> = HashMap::new();
    for tag in all_tags {
        if !file_idx.contains_key(&tag.file) {
            file_idx.insert(tag.file.clone(), file_set.len());
            file_set.push(tag.file.clone());
        }
    }

    let mut defines: HashMap<String, HashSet<usize>> = HashMap::new();
    let mut refs: HashMap<String, Vec<(usize, u32)>> = HashMap::new();
    let mut file_tag_map: Vec<Vec<Tag>> = vec![Vec::new(); file_set.len()];

    for tag in all_tags {
        let fi = file_idx[&tag.file];
        file_tag_map[fi].push(tag.clone());
        if tag.is_def {
            defines.entry(tag.name.clone()).or_default().insert(fi);
        } else {
            refs.entry(tag.name.clone()).or_default().push((fi, 1));
        }
    }

    // Collapse duplicate reference counts
    for (_name, ref_list) in refs.iter_mut() {
        let mut counts: HashMap<usize, u32> = HashMap::new();
        for &(fi, _) in ref_list.iter() {
            *counts.entry(fi).or_default() += 1;
        }
        *ref_list = counts.into_iter().collect();
    }

    let mut edges: Vec<(usize, usize, f64)> = Vec::new();
    let mut self_edges: HashSet<(usize, usize)> = HashSet::new();

    for (name, def_set) in &defines {
        if let Some(ref_list) = refs.get(name) {
            for &(ref_fi, count) in ref_list {
                if def_set.contains(&ref_fi) {
                    continue;
                }
                let weight = (count as f64).sqrt();
                for &def_fi in def_set {
                    if !self_edges.insert((ref_fi, def_fi)) {
                        continue;
                    }
                    edges.push((ref_fi, def_fi, weight));
                }
            }
        }
    }

    DepGraph {
        file_count: file_set.len(),
        file_names: file_set,
        edges,
        file_tags: file_tag_map,
    }
}

// ── PageRank ───────────────────────────────────────────────────────────────

fn pagerank(
    num_nodes: usize,
    edges: &[(usize, usize, f64)],
    personalization: &[f64],
    damping: f64,
    iterations: usize,
) -> Vec<f64> {
    if num_nodes == 0 {
        return Vec::new();
    }

    let mut rank = personalization.to_vec();
    let mut new_rank = vec![0.0; num_nodes];

    let mut out_weight = vec![0.0; num_nodes];
    let mut in_edges: Vec<Vec<(usize, f64)>> = vec![Vec::new(); num_nodes];
    for &(from, to, weight) in edges {
        out_weight[from] += weight;
        in_edges[to].push((from, weight));
    }

    for _ in 0..iterations {
        let dangling_sum: f64 = rank
            .iter()
            .enumerate()
            .filter(|(i, _)| out_weight[*i] == 0.0)
            .map(|(_, r)| r)
            .sum();
        let teleport = (1.0 - damping + damping * dangling_sum) / num_nodes as f64;

        for i in 0..num_nodes {
            let mut score = 0.0_f64;
            for &(from, weight) in &in_edges[i] {
                score += damping * rank[from] * weight / out_weight[from];
            }
            new_rank[i] = score + teleport * personalization[i] * num_nodes as f64;
        }

        let total: f64 = new_rank.iter().sum();
        if total > 0.0 {
            for r in &mut new_rank {
                *r /= total;
            }
        }

        rank.copy_from_slice(&new_rank);
    }

    rank
}

// ── ranking → ranked definitions ───────────────────────────────────────────

fn rank_definitions(graph: &DepGraph, pagerank_scores: &[f64]) -> Vec<RankedDef> {
    let mut def_rank: HashMap<(String, String), (f64, usize, String)> = HashMap::new();

    let defines_map = build_defines_map(&graph.file_tags);

    for (name, def_files) in &defines_map {
        for &df in def_files {
            if df >= graph.file_names.len() {
                continue;
            }
            let file = graph.file_names[df].clone();
            let key = (file.clone(), name.clone());

            let ref_count = count_ref_files(name, &graph.file_tags);
            let weight = (ref_count.max(1) as f64).sqrt();

            let entry = def_rank.entry(key).or_insert_with(|| {
                let tag = graph.file_tags[df]
                    .iter()
                    .find(|t| t.is_def && t.name == *name)
                    .cloned()
                    .unwrap_or_else(|| Tag {
                        file: file.clone(),
                        name: name.clone(),
                        is_def: true,
                        line: 0,
                        snippet: String::new(),
                    });
                (0.0, tag.line, tag.snippet)
            });
            entry.0 += pagerank_scores[df] * weight;
        }
    }

    let mut ranked: Vec<RankedDef> = def_rank
        .into_iter()
        .map(|((file, name), (rank, line, snippet))| RankedDef {
            file,
            name,
            rank,
            line,
            snippet,
        })
        .collect();

    ranked.sort_by(|a, b| {
        b.rank
            .partial_cmp(&a.rank)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.name.cmp(&b.name))
    });

    ranked
}

fn build_defines_map(file_tags: &[Vec<Tag>]) -> HashMap<String, HashSet<usize>> {
    let mut map: HashMap<String, HashSet<usize>> = HashMap::new();
    for (fi, tags) in file_tags.iter().enumerate() {
        for tag in tags {
            if tag.is_def {
                map.entry(tag.name.clone()).or_default().insert(fi);
            }
        }
    }
    map
}

fn count_ref_files(name: &str, file_tags: &[Vec<Tag>]) -> usize {
    let mut files: HashSet<usize> = HashSet::new();
    for (fi, tags) in file_tags.iter().enumerate() {
        for tag in tags {
            if !tag.is_def && tag.name == name {
                files.insert(fi);
            }
        }
    }
    files.len()
}

// ── rendering ──────────────────────────────────────────────────────────────

fn render_context_map(
    ranked: &[RankedDef],
    mentioned_terms: &[String],
    max_tokens: usize,
) -> String {
    if mentioned_terms.is_empty() || max_tokens < 256 {
        return render_repo_map(ranked, max_tokens);
    }

    let terms = normalized_terms(mentioned_terms);
    let mut evidence: Vec<(u32, &RankedDef)> = ranked
        .iter()
        .filter_map(|definition| {
            let score = task_match_score(&definition.name, &terms) * 4
                + task_match_score(&definition.file, &terms) * 2
                + task_match_score(&definition.snippet, &terms);
            (score > 0).then_some((score, definition))
        })
        .collect();
    evidence.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| {
                b.1.rank
                    .partial_cmp(&a.1.rank)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.1.file.cmp(&b.1.file))
            .then_with(|| a.1.line.cmp(&b.1.line))
    });
    if evidence.is_empty() {
        return render_repo_map(ranked, max_tokens);
    }

    let evidence_budget = (max_tokens / 3).min(384);
    let mut output = render_task_evidence(&evidence, evidence_budget);
    output.push_str("\n\n");
    output.push_str(&render_repo_map(
        ranked,
        max_tokens.saturating_sub(evidence_budget),
    ));
    output
}

fn render_task_evidence(evidence: &[(u32, &RankedDef)], max_tokens: usize) -> String {
    let mut lines = vec!["Task evidence (task-linked definitions before global rank):".to_string()];
    let mut current_file = None;
    let mut token_est = estimate_tokens(&lines[0], 3.5);
    let mut seen = HashSet::new();

    for (_, definition) in evidence {
        if !seen.insert((&definition.file, &definition.name)) {
            continue;
        }
        let file_changed = current_file != Some(definition.file.as_str());
        let file_line = definition.file.clone();
        let def_line = format!("  L{:>4}  {}", definition.line, definition.snippet);
        let file_tokens = if file_changed {
            estimate_tokens(&file_line, 3.5)
        } else {
            0
        };
        let added = estimate_tokens(&def_line, 3.5) + file_tokens;
        if token_est + added > max_tokens {
            lines.push("  ... (task evidence truncated)".into());
            break;
        }
        if file_changed {
            lines.push(file_line);
            current_file = Some(definition.file.as_str());
        }
        lines.push(def_line);
        token_est += added;
    }
    lines.join("\n")
}

fn render_repo_map(ranked: &[RankedDef], max_tokens: usize) -> String {
    let mut file_order: Vec<&str> = Vec::new();
    let mut seen_files: HashSet<&str> = HashSet::new();
    let mut file_defs: HashMap<&str, Vec<&RankedDef>> = HashMap::new();

    for rd in ranked {
        if seen_files.insert(&rd.file) {
            file_order.push(&rd.file);
        }
        file_defs.entry(&rd.file).or_default().push(rd);
    }

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "Repo map ({:.1}k tokens):",
        max_tokens as f64 / 1000.0
    ));
    lines.push(String::new());

    let mut token_est = 0_usize;
    let mut truncated = false;
    let chars_per_token = 3.5_f64;

    for file in file_order.iter() {
        if token_est > max_tokens {
            truncated = true;
            break;
        }
        let f_line = (*file).to_string();
        token_est += estimate_tokens(&f_line, chars_per_token);
        lines.push(f_line);

        if let Some(defs) = file_defs.get(file) {
            for rd in defs {
                let def_line = format!(
                    "  {:>10} L{:>4}  {}",
                    format_rank(rd.rank),
                    rd.line,
                    rd.snippet,
                );
                let t = estimate_tokens(&def_line, chars_per_token);
                if token_est + t > max_tokens {
                    let file_pos = file_order.iter().position(|f| *f == *file).unwrap_or(0);
                    let remaining_defs = file_defs
                        .get(file)
                        .map(|d| d.iter().filter(|x| x.rank < rd.rank).count())
                        .unwrap_or(0);
                    let remaining_files = file_order.len() - file_pos;
                    lines.push(format!(
                        "  ... (+{} more symbols across {} files omitted)",
                        remaining_defs + remaining_files * 3,
                        remaining_files,
                    ));
                    truncated = true;
                    break;
                }
                token_est += t;
                lines.push(def_line);
            }
            if truncated {
                break;
            }
        }
    }

    if truncated {
        lines.push(String::new());
        lines.push("(repo map truncated — raise `max_map_tokens` for more)".into());
    }

    lines.join("\n")
}

fn format_rank(rank: f64) -> String {
    if rank <= 0.0 {
        return "··········".to_string();
    }
    let scaled = ((rank * 10.0).min(10.0)) as usize;
    let clamped = scaled.max(1);
    let full = "█".repeat(clamped);
    let empty = "·".repeat(10_usize.saturating_sub(clamped));
    format!("{}{}", full, empty)
}

fn estimate_tokens(text: &str, chars_per_token: f64) -> usize {
    (text.chars().count() as f64 / chars_per_token).ceil() as usize
}

// ── public API ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct RepoMapOptions {
    pub max_map_tokens: usize,
    pub min_source_files: usize,
    pub chat_files: Vec<PathBuf>,
    pub mentioned_terms: Vec<String>,
}

impl Default for RepoMapOptions {
    fn default() -> Self {
        Self {
            max_map_tokens: 1024,
            min_source_files: 20,
            chat_files: Vec::new(),
            mentioned_terms: Vec::new(),
        }
    }
}

pub async fn generate_repo_map(workspace_root: &Path, options: &RepoMapOptions) -> Option<String> {
    let file_paths =
        select_source_files(walk_source_files(workspace_root), &options.mentioned_terms).await;
    if file_paths.len() < options.min_source_files {
        return None;
    }

    let tags = extract_all_tags(&file_paths).await;
    if tags.is_empty() {
        return None;
    }

    let chat_set: HashSet<String> = options
        .chat_files
        .iter()
        .filter_map(|p| {
            p.strip_prefix(workspace_root)
                .ok()
                .map(|r| normalize_rel_path(&r.to_string_lossy()))
        })
        .collect();

    let graph = build_dep_graph(&tags);

    if graph.file_count == 0 {
        return None;
    }

    let personalization = build_personalization(
        graph.file_count,
        &graph.file_names,
        &graph.file_tags,
        &chat_set,
        &options.mentioned_terms,
    );

    let ranks = pagerank(graph.file_count, &graph.edges, &personalization, 0.85, 30);

    let ranked = rank_definitions(&graph, &ranks);

    let ranked: Vec<RankedDef> = ranked
        .into_iter()
        .filter(|rd| !chat_set.contains(&rd.file))
        .collect();

    if ranked.is_empty() {
        return None;
    }

    Some(render_context_map(
        &ranked,
        &options.mentioned_terms,
        options.max_map_tokens,
    ))
}

// ── file walking ───────────────────────────────────────────────────────────

fn walk_source_files(root: &Path) -> Vec<(String, PathBuf)> {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let ext_set: HashSet<&str> = SOURCE_EXTENSIONS.iter().copied().collect();

    // Seed with the root itself so the loop below collects *its* files too. Walking the root's
    // entries separately here (pushing only directories) silently dropped every source file
    // sitting directly in the workspace — `main.rs` in a single-crate repo, a flat Python or Go
    // entry point — which is exactly what a cold-start map most needs to name.
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];

    // The root now consumes one level, so the cap is one deeper than the 8 it replaced — the same
    // reach below the workspace as before.
    let mut depth = 0;
    while !stack.is_empty() && files.len() < MAX_SCAN_FILES {
        depth += 1;
        if depth > 9 {
            break;
        }
        let mut next: Vec<PathBuf> = Vec::new();
        for dir in &stack {
            if let Ok(entries) = std::fs::read_dir(dir) {
                let mut entries: Vec<_> = entries.flatten().collect();
                entries.sort_by_key(|entry| entry.file_name());
                for entry in entries {
                    let path = entry.path();
                    let fname = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    if fname.starts_with('.')
                        || fname == "node_modules"
                        || fname == "target"
                        || fname == "__pycache__"
                    {
                        continue;
                    }
                    if path.is_dir() {
                        next.push(path);
                    } else if let Some(ext) = path.extension() {
                        let ext = ext.to_string_lossy().to_string();
                        if ext_set.contains(ext.as_str())
                            && let Ok(meta) = path.metadata()
                            && meta.len() < MAX_FILE_SIZE as u64
                        {
                            let rel = normalize_rel_path(
                                &path.strip_prefix(root).unwrap_or(&path).to_string_lossy(),
                            );
                            files.push((rel, path));
                        }
                    }
                }
            }
            if files.len() >= MAX_SCAN_FILES {
                break;
            }
        }
        next.sort();
        stack = next;
    }

    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

fn normalize_rel_path(path: &str) -> String {
    path.replace('\\', "/")
}

async fn select_source_files(
    mut files: Vec<(String, PathBuf)>,
    mentioned_terms: &[String],
) -> Vec<(String, PathBuf)> {
    if files.len() <= MAX_FILES {
        return files;
    }
    if mentioned_terms.is_empty() {
        files.truncate(MAX_FILES);
        return files;
    }

    let terms = normalized_terms(mentioned_terms);
    let mut scored = Vec::with_capacity(files.len());
    for (rel, path) in files {
        let source = tokio::fs::read_to_string(&path).await.unwrap_or_default();
        let score = task_match_score(&rel, &terms) * 4 + task_match_score(&source, &terms);
        scored.push((score, rel, path));
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.truncate(MAX_FILES);
    scored
        .into_iter()
        .map(|(_, rel, path)| (rel, path))
        .collect()
}

// ── tag extraction (sequential I/O, blocking parse) ────────────────────────

async fn extract_all_tags(file_paths: &[(String, PathBuf)]) -> Vec<Tag> {
    let mut all_tags: Vec<Tag> = Vec::new();

    for (rel_path, abs_path) in file_paths {
        let source = match tokio::fs::read_to_string(abs_path).await {
            Ok(s) => s,
            Err(_) => continue,
        };
        let (lang_name, lang) = match detect_lang(rel_path) {
            Some(l) => l,
            None => continue,
        };
        let rel = rel_path.clone();
        let tags =
            tokio::task::spawn_blocking(move || extract_tags(&rel, &source, lang_name, &lang))
                .await
                .unwrap_or_default();

        all_tags.extend(tags);
    }

    all_tags
}

// ── personalization vector ─────────────────────────────────────────────────

fn build_personalization(
    num_files: usize,
    file_names: &[String],
    file_tags: &[Vec<Tag>],
    chat_set: &HashSet<String>,
    mentioned_terms: &[String],
) -> Vec<f64> {
    let base = 1.0 / num_files.max(1) as f64;
    let mut vec = vec![base; num_files];
    let terms = normalized_terms(mentioned_terms);

    for (i, fname) in file_names.iter().enumerate() {
        let mut boost = 1.0_f64;

        if chat_set.contains(fname) {
            boost *= 10.0;
        }

        let path_score = task_match_score(fname, &terms);
        let symbol_score = file_tags
            .get(i)
            .into_iter()
            .flatten()
            .filter(|tag| tag.is_def)
            .map(|tag| task_match_score(&tag.name, &terms))
            .max()
            .unwrap_or(0);
        if path_score > 0 || symbol_score > 0 {
            boost *= 3.0 + f64::from((path_score + symbol_score).min(9));
        }

        vec[i] *= boost;
    }

    let total: f64 = vec.iter().sum();
    if total > 0.0 {
        for v in &mut vec {
            *v /= total;
        }
    }

    vec
}

fn normalized_terms(mentioned_terms: &[String]) -> Vec<String> {
    let mut terms: Vec<String> = mentioned_terms
        .iter()
        .map(|term| term.to_ascii_lowercase())
        .filter(|term| !term.is_empty())
        .collect();
    terms.sort();
    terms.dedup();
    terms
}

fn task_match_score(candidate: &str, terms: &[String]) -> u32 {
    let candidate = candidate.to_ascii_lowercase();
    terms
        .iter()
        .map(|term| {
            if candidate == *term {
                12
            } else if candidate.contains(term) {
                4
            } else {
                let singular = term.strip_suffix('s').filter(|term| term.len() >= 4);
                if singular.is_some_and(|term| candidate.contains(term)) {
                    2
                } else {
                    0
                }
            }
        })
        .sum::<u32>()
        .min(64)
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Source files sitting directly in the workspace root belong in the map.
    ///
    /// Single-crate Rust repos put `main.rs`/`lib.rs` at the top level, and plenty of Python and
    /// Go projects keep their entry point beside the manifest — dropping those hides exactly the
    /// files a cold-start map most needs to name.
    #[test]
    fn walk_collects_source_files_directly_under_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("z_last.rs"), "fn z_last() {}").unwrap();
        std::fs::write(root.join("a_first.rs"), "fn a_first() {}").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("nested.rs"), "fn nested() {}").unwrap();

        let found: Vec<String> = walk_source_files(root)
            .into_iter()
            .map(|(rel, _)| rel.replace('\\', "/"))
            .collect();

        assert!(
            found.windows(2).all(|pair| pair[0] <= pair[1]),
            "walk order must be deterministic: {found:?}"
        );

        assert!(
            found.iter().any(|f| f == "src/nested.rs"),
            "nested file should be found: {found:?}"
        );
        assert!(
            found.iter().any(|f| f == "main.rs"),
            "root-level source file should be found: {found:?}"
        );
    }

    #[test]
    fn test_pagerank_simple() {
        let edges = vec![(0, 1, 1.0)];
        let personalization = vec![0.5, 0.5];
        let ranks = pagerank(2, &edges, &personalization, 0.85, 50);

        assert!(ranks[1] > ranks[0]);
    }

    #[test]
    fn test_detect_lang_rust() {
        let (name, _lang) = detect_lang("src/main.rs").unwrap();
        assert_eq!(name, "rust");
    }

    #[test]
    fn test_detect_lang_python() {
        let (name, _lang) = detect_lang("app/views.py").unwrap();
        assert_eq!(name, "python");
    }

    #[test]
    fn test_detect_lang_typescript() {
        let (name, _lang) = detect_lang("src/App.tsx").unwrap();
        assert_eq!(name, "tsx");
    }

    #[test]
    fn test_detect_lang_go() {
        let (name, _lang) = detect_lang("pkg/handler.go").unwrap();
        assert_eq!(name, "go");
    }

    #[test]
    fn test_detect_lang_unknown() {
        // Use a non-docs/ path: docs_meta check-stale-rs scans crates for
        // docs/**/*.md strings and requires the target to exist with exact case.
        assert!(detect_lang("notes/readme.md").is_none());
    }

    #[test]
    fn test_extract_rust_tags() {
        let source = r#"
struct App {
    name: String,
}

impl App {
    fn run(&self) {
        self.init();
    }

    fn init(&self) {}
}

fn main() {
    let app = App { name: "hello".into() };
    app.run();
}
"#;
        let lang: Language = tree_sitter_rust::LANGUAGE.into();
        let tags = extract_tags("test.rs", source, "rust", &lang);

        let defs: Vec<_> = tags.iter().filter(|t| t.is_def).collect();
        let refs: Vec<_> = tags.iter().filter(|t| !t.is_def).collect();

        let def_names: HashSet<_> = defs.iter().map(|t| t.name.as_str()).collect();
        assert!(def_names.contains("App"));
        assert!(def_names.contains("run"));
        assert!(def_names.contains("init"));
        assert!(def_names.contains("main"));

        let ref_names: HashSet<_> = refs.iter().map(|t| t.name.as_str()).collect();
        assert!(ref_names.contains("init"));
        assert!(ref_names.contains("run"));
    }

    #[test]
    fn test_extract_python_tags() {
        let source = r#"
class App:
    def run(self):
        self.init()

    def init(self):
        pass

def main():
    app = App()
    app.run()
"#;
        let lang: Language = tree_sitter_python::LANGUAGE.into();
        let tags = extract_tags("test.py", source, "python", &lang);

        let def_names: HashSet<_> = tags
            .iter()
            .filter(|t| t.is_def)
            .map(|t| t.name.as_str())
            .collect();
        assert!(def_names.contains("App"));
        assert!(def_names.contains("run"));
        assert!(def_names.contains("init"));
        assert!(def_names.contains("main"));
    }

    #[test]
    fn test_build_dep_graph_empty() {
        let graph = build_dep_graph(&[]);
        assert_eq!(graph.file_count, 0);
    }

    #[test]
    fn test_render_repo_map() {
        let ranked = vec![
            RankedDef {
                file: "src/main.rs".into(),
                name: "main".into(),
                rank: 0.5,
                line: 10,
                snippet: "fn main() {".into(),
            },
            RankedDef {
                file: "src/lib.rs".into(),
                name: "run".into(),
                rank: 0.3,
                line: 42,
                snippet: "pub fn run() {".into(),
            },
        ];
        let output = render_repo_map(&ranked, 1024);
        assert!(output.contains("src/main.rs"));
        assert!(output.contains("src/lib.rs"));
        assert!(output.contains("fn main()"));
        assert!(output.contains("pub fn run()"));
    }

    #[test]
    fn test_personalization_chat_files() {
        let files = [
            "src/main.rs".to_string(),
            "src/lib.rs".to_string(),
            "tests/test.rs".to_string(),
        ];
        let mut chat = HashSet::new();
        chat.insert("src/main.rs".to_string());
        let tags = vec![Vec::new(), Vec::new(), Vec::new()];
        let vec = build_personalization(3, &files, &tags, &chat, &[]);

        assert!(vec[0] > vec[1]);
        assert!(vec[0] > vec[2]);
    }

    #[test]
    fn task_symbol_boosts_its_defining_file() {
        let files = ["src/alpha.rs".to_string(), "src/beta.rs".to_string()];
        let tags = vec![
            Vec::new(),
            vec![Tag {
                file: "src/beta.rs".into(),
                name: "ResourceLink".into(),
                is_def: true,
                line: 7,
                snippet: "pub struct ResourceLink {".into(),
            }],
        ];
        let vec =
            build_personalization(2, &files, &tags, &HashSet::new(), &["ResourceLink".into()]);
        assert!(vec[1] > vec[0], "{vec:?}");
    }

    #[test]
    fn task_terms_keep_code_names_and_drop_common_instructions() {
        let terms = extract_task_terms(
            "Implement ResourceLink and ContentBlock handling in turbomcp/crates/types/src/content.rs with tests",
        );
        assert!(terms.contains(&"ResourceLink".to_string()), "{terms:?}");
        assert!(terms.contains(&"ContentBlock".to_string()), "{terms:?}");
        assert!(
            terms.contains(&"turbomcp/crates/types/src/content.rs".to_string()),
            "{terms:?}"
        );
        assert!(
            !terms
                .iter()
                .any(|term| term.eq_ignore_ascii_case("implement"))
        );
        assert!(!terms.iter().any(|term| term.eq_ignore_ascii_case("with")));
    }

    #[test]
    fn code_like_terms_survive_a_long_task() {
        let mut task = (0..40)
            .map(|index| format!("ordinaryword{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        task.push_str(" ResourceLink src/content.rs");
        let terms = extract_task_terms(&task);
        assert_eq!(terms.len(), 32);
        assert!(terms.contains(&"ResourceLink".to_string()), "{terms:?}");
        assert!(terms.contains(&"src/content.rs".to_string()), "{terms:?}");
    }

    #[tokio::test]
    async fn task_match_survives_the_global_file_cap() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let filler = root.join("crates/filler/src");
        std::fs::create_dir_all(&filler).unwrap();
        for index in 0..MAX_FILES {
            std::fs::write(
                filler.join(format!("file_{index:03}.rs")),
                format!("fn filler_{index}() {{}}"),
            )
            .unwrap();
        }
        let authoritative = root.join("turbomcp/crates/turbomcp-types/src");
        std::fs::create_dir_all(&authoritative).unwrap();
        std::fs::write(
            authoritative.join("content.rs"),
            "pub struct ResourceLink { pub uri: String }",
        )
        .unwrap();

        let selected = select_source_files(walk_source_files(root), &["ResourceLink".into()]).await;
        assert_eq!(selected.len(), MAX_FILES);
        assert!(
            selected.iter().any(|(path, _)| {
                path.replace('\\', "/") == "turbomcp/crates/turbomcp-types/src/content.rs"
            }),
            "task evidence must displace an unrelated filler file"
        );
    }

    #[tokio::test]
    async fn task_evidence_precedes_global_rank() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("turbomcp/crates/turbomcp-types/src")).unwrap();
        std::fs::create_dir_all(root.join("crates/app/src")).unwrap();
        std::fs::write(
            root.join("turbomcp/crates/turbomcp-types/src/content.rs"),
            "pub struct ResourceLink { pub uri: String }\npub enum ContentBlock { Text }",
        )
        .unwrap();
        std::fs::write(root.join("crates/app/src/lib.rs"), "pub fn unrelated() {}").unwrap();

        let output = generate_repo_map(
            root,
            &RepoMapOptions {
                min_source_files: 1,
                mentioned_terms: vec!["ResourceLink".into(), "ContentBlock".into()],
                ..Default::default()
            },
        )
        .await
        .expect("repo map");
        let evidence = output.find("Task evidence").expect("task evidence heading");
        let path = output
            .find("turbomcp/crates/turbomcp-types/src/content.rs")
            .expect("authoritative path");
        let global = output.find("Repo map").expect("global map heading");
        assert!(evidence < path && path < global, "{output}");
        assert!(output.contains("ResourceLink"), "{output}");
        assert!(output.contains("ContentBlock"), "{output}");
    }

    #[test]
    fn test_render_truncation() {
        let mut ranked: Vec<RankedDef> = (0..100)
            .map(|i| RankedDef {
                file: format!("src/file{}.rs", i),
                name: format!("fn_{}", i),
                rank: 0.01,
                line: i,
                snippet: format!("fn fn_{}() {{ /* long comment {} */ }}", i, "x".repeat(80)),
            })
            .collect();
        ranked.sort_by(|a, b| {
            b.rank
                .partial_cmp(&a.rank)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let output = render_repo_map(&ranked, 200);
        assert!(output.contains("truncated") || output.contains("omitted"));

        let output = render_repo_map(&ranked[..10], 10000);
        assert!(!output.contains("truncated"));
        assert!(!output.contains("omitted"));
    }

    mod proptests {
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn format_rank_never_panics(r: f64) {
                let _ = super::format_rank(r);
            }

            #[test]
            fn format_rank_always_10_chars(r: f64) {
                let s = super::format_rank(r);
                assert_eq!(s.chars().count(), 10, "rank={r} produced '{}'", s);
            }

            #[test]
            fn format_rank_non_positive_is_all_dots(r in proptest::num::f64::NORMAL) {
                let r = -r.abs();
                let s = super::format_rank(r);
                assert!(s.chars().all(|c| c == '·'), "negative rank {r} produced '{s}'");
            }

            #[test]
            fn estimate_tokens_never_panics(s in "\\PC*", c in 0.1_f64..100.0) {
                let _ = super::estimate_tokens(&s, c);
            }

            #[test]
            fn estimate_tokens_empty_is_zero(c in 0.1_f64..100.0) {
                assert_eq!(super::estimate_tokens("", c), 0);
            }

            #[test]
            fn estimate_tokens_monotonic(s in "\\PC{0,50}", c in 0.1_f64..100.0) {
                let base = super::estimate_tokens(&s, c);
                let longer = super::estimate_tokens(&format!("{s}x"), c);
                assert!(longer >= base, "longer string had fewer tokens ({longer} < {base})");
            }
        }
    }
}
#[cfg(test)]
#[path = "repo_map_survivor_tests.rs"]
mod survivor_tests;
