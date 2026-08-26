//! Survivor tests for `repo_map.rs`.
//!
//! Wired as a sibling module (`#[path]`) so the private pipeline — graph,
//! PageRank, personalization, rendering — stays directly assertable. Numeric
//! kernels are pinned against independently computed golden vectors; rendering
//! is pinned on exact inclusion, omission-message arithmetic, and budget
//! boundaries built with the crate's own `estimate_tokens` oracle.

use super::*;

fn approx(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() < 1e-6,
        "{what}: expected {expected}, got {actual}"
    );
}

fn tag(file: &str, name: &str, is_def: bool, line: usize) -> Tag {
    Tag {
        file: file.to_string(),
        name: name.to_string(),
        is_def,
        line,
        snippet: String::new(),
    }
}

// ── pagerank ────────────────────────────────────────────────────────────────

/// Golden converged distributions, computed independently of the Rust code.
///
/// Config A exercises weighted edges plus a dangling node (its unspent mass
/// must re-enter through the teleport term). Config B exercises a cycle.
/// Config C pins the zero-total guard: an all-zero personalization must stay
/// exactly zero rather than divide by zero into NaN.
#[test]
fn pagerank_matches_golden_distributions() {
    let a = pagerank(3, &[(0, 1, 1.0), (0, 2, 3.0)], &[0.5, 0.25, 0.25], 0.85, 50);
    approx(a[0], 0.350_877_193, "A node0");
    approx(a[1], 0.250_000_000, "A node1");
    approx(a[2], 0.399_122_807, "A node2");
    assert!(
        a.iter().all(|r| r.is_finite() && *r > 0.0),
        "every rank stays positive: {a:?}"
    );

    let b = pagerank(
        4,
        &[(0, 1, 2.0), (1, 2, 1.0), (2, 0, 1.0), (2, 3, 1.0)],
        &[0.25; 4],
        0.85,
        30,
    );
    approx(b[0], 0.213_762_353, "B node0");
    approx(b[1], 0.264_621_935, "B node1");
    approx(b[2], 0.307_853_360, "B node2");
    approx(b[3], 0.213_762_353, "B node3");

    let c = pagerank(2, &[(0, 1, 1.0)], &[0.0, 0.0], 0.85, 5);
    assert!(
        c.iter().all(|r| r.is_finite()),
        "zero personalization must not produce NaN: {c:?}"
    );
}

// ── personalization ─────────────────────────────────────────────────────────

/// A chat file is boosted by a factor of ten, not an additive nudge: with two
/// files and nothing else in play the shares must be exactly 10/11 vs 1/11.
#[test]
fn chat_boost_multiplies_by_ten_exactly() {
    let files = ["a.rs".to_string(), "c.rs".to_string()];
    let mut chat = HashSet::new();
    chat.insert("c.rs".to_string());
    let vec = build_personalization(2, &files, &[Vec::new(), Vec::new()], &chat, &[]);
    let ratio = vec[1] / vec[0];
    approx(ratio, 10.0, "chat file holds exactly 10x the share");
}

/// Task-term boosts multiply the base share: a path-and-symbol match lifts one
/// file to (3 + 8)/12 of the mass while the unmatched file keeps 1/12.
#[test]
fn task_boost_golden_vector() {
    let files = ["src/widget.rs".to_string(), "other.rs".to_string()];
    let tags = vec![
        vec![tag("src/widget.rs", "widget_maker", true, 3)],
        Vec::new(),
    ];
    let vec = build_personalization(2, &files, &tags, &HashSet::new(), &["widget".to_string()]);
    // path_score(widget.rs)=4 (contains), symbol_score=4 (widget_maker) -> boost 3+8=11.
    approx(vec[0], 11.0 / 12.0, "matched share");
    approx(vec[1], 1.0 / 12.0, "unmatched share");
    assert!(vec[0] > vec[1]);
}

// ── graph construction ──────────────────────────────────────────────────────

/// Reference counts collapse per file, edge weights are sqrt of the collapsed
/// count, and a (ref_file, def_file) pair is emitted once regardless of how
/// many defined names share it.
#[test]
fn dep_graph_weights_and_deduplication() {
    // Every name owns a distinct (ref_file, def_file) pair so edge weights
    // cannot depend on HashSet iteration order: Widget is defined alone in
    // d.rs (two collapsed refs from a.rs -> sqrt(2)); Gadget (b.rs) and
    // Ziptie (b.rs) share the a->b pair, each worth 1.0 whichever claims it;
    // Ziptie is additionally referenced from c.rs.
    let tags = vec![
        tag("b.rs", "Gadget", true, 1),
        tag("b.rs", "Ziptie", true, 2),
        tag("d.rs", "Widget", true, 3),
        tag("a.rs", "Widget", false, 4),
        tag("a.rs", "Widget", false, 5),
        tag("a.rs", "Gadget", false, 6),
        tag("a.rs", "Ziptie", false, 7),
        tag("c.rs", "Ziptie", false, 8),
    ];
    let graph = build_dep_graph(&tags);

    let idx = |name: &str| graph.file_names.iter().position(|f| f == name).unwrap();
    let (a, b, c, d) = (idx("a.rs"), idx("b.rs"), idx("c.rs"), idx("d.rs"));
    assert_eq!(
        graph.edges.len(),
        3,
        "one edge per (ref,def) pair: {:?}",
        graph.edges
    );
    assert!(
        graph
            .edges
            .iter()
            .any(|&(f, t, w)| f == a && t == d && (w - 2.0_f64.sqrt()).abs() < 1e-9),
        "Widget a->d carries sqrt(2): {:?}",
        graph.edges
    );
    assert!(
        graph
            .edges
            .iter()
            .any(|&(f, t, w)| f == a && t == b && (w - 1.0).abs() < 1e-9),
        "shared a->b pair emitted once at weight 1: {:?}",
        graph.edges
    );
    assert!(
        graph
            .edges
            .iter()
            .any(|&(f, t, w)| f == c && t == b && (w - 1.0).abs() < 1e-9),
        "Ziptie c->b carries 1.0: {:?}",
        graph.edges
    );
}

// ── ranking ─────────────────────────────────────────────────────────────────

/// A definition's score is its file's PageRank times sqrt(referring-file
/// count): two referring files lift Widget to sqrt(2) exactly.
#[test]
fn rank_definitions_multiplies_pagerank_by_reference_weight() {
    let tags = vec![
        tag("b.rs", "Widget", true, 1),
        tag("a.rs", "Widget", false, 2),
        tag("c.rs", "Widget", false, 3),
    ];
    let graph = build_dep_graph(&tags);
    let scores = vec![1.0, 0.0, 0.0];
    let ranked = rank_definitions(&graph, &scores);
    assert_eq!(ranked.len(), 1, "{ranked:?}");
    approx(ranked[0].rank, 2.0_f64.sqrt(), "Widget rank");
}

/// `count_ref_files` counts *distinct files* holding a non-definition tag,
/// ignoring defining files and unrelated names.
#[test]
fn count_ref_files_counts_distinct_referencing_files_only() {
    let file_tags = vec![
        vec![
            tag("a.rs", "Widget", false, 1),
            tag("a.rs", "Widget", false, 2),
        ],
        vec![tag("b.rs", "Widget", true, 1)],
        vec![
            tag("c.rs", "Widget", false, 5),
            tag("c.rs", "Gadget", false, 6),
        ],
    ];
    assert_eq!(count_ref_files("Widget", &file_tags), 2);
    assert_eq!(count_ref_files("Gadget", &file_tags), 1);
    assert_eq!(count_ref_files("missing", &file_tags), 0);
}

// ── tag extraction ──────────────────────────────────────────────────────────

/// Names shorter than two characters or longer than eighty are dropped, and
/// reported lines are one-based.
#[test]
fn extract_tags_enforces_name_length_bounds_and_one_based_lines() {
    let long80 = "n".repeat(80);
    let long81 = "n".repeat(81);
    let source = format!(
        "fn filler0() {{}}\nfn filler1() {{}}\nfn filler2() {{}}\nfn filler3() {{}}\nfn a() {{}}\nfn ab() {{}}\nfn {long80}() {{}}\nfn {long81}() {{}}\n"
    );
    let lang: Language = tree_sitter_rust::LANGUAGE.into();
    let tags = extract_tags("t.rs", &source, "rust", &lang);
    let defs: Vec<&str> = tags
        .iter()
        .filter(|t| t.is_def)
        .map(|t| t.name.as_str())
        .collect();
    assert!(!defs.contains(&"a"), "1-char name dropped: {defs:?}");
    assert!(defs.contains(&"ab"), "2-char name kept: {defs:?}");
    assert!(
        defs.iter().any(|d| d.len() == 80),
        "80-char name kept: {defs:?}"
    );
    assert!(
        !defs.iter().any(|d| d.len() > 80),
        "81+-char names dropped: {defs:?}"
    );
    let ab = tags.iter().find(|t| t.name == "ab").unwrap();
    assert_eq!(ab.line, 6, "one-based line: {:?}", ab.line);
}

// ── language detection and per-language queries ─────────────────────────────

#[test]
fn detect_lang_covers_ts_variants() {
    assert!(detect_lang("src/app.ts").is_some(), ".ts detected");
    assert!(detect_lang("src/app.js").is_some(), ".js detected");
    assert!(detect_lang("src/app.jsx").is_some(), ".jsx detected");
}

/// The TypeScript and Go query sources stay loadable and productive.
#[test]
fn extract_tags_supports_typescript_and_go() {
    let ts_source = "function hello(): void {\n  world();\n}\nfunction world(): void {}\n";
    let (ts_name, ts_lang) = detect_lang("a.ts").unwrap();
    let ts_tags = extract_tags("a.ts", ts_source, ts_name, &ts_lang);
    let ts_defs: Vec<&str> = ts_tags
        .iter()
        .filter(|t| t.is_def)
        .map(|t| t.name.as_str())
        .collect();
    assert!(
        ts_defs.contains(&"hello") && ts_defs.contains(&"world"),
        "typescript definitions found: {ts_defs:?} ({ts_name})"
    );

    let go_source = "package main\n\nfunc main() {\n\thelper()\n}\n\nfunc helper() {}\n";
    let (go_name, go_lang) = detect_lang("m.go").unwrap();
    let go_tags = extract_tags("m.go", go_source, go_name, &go_lang);
    let go_defs: Vec<&str> = go_tags
        .iter()
        .filter(|t| t.is_def)
        .map(|t| t.name.as_str())
        .collect();
    assert!(
        go_defs.contains(&"main") && go_defs.contains(&"helper"),
        "go definitions found: {go_defs:?} ({go_name})"
    );
    assert!(
        go_tags.iter().any(|t| !t.is_def && t.name == "helper"),
        "helper call captured as reference: {:?}",
        go_tags
    );
}

// ── task terms ──────────────────────────────────────────────────────────────

/// Words with neither case nor separators classify as code-like only when
/// they are short: A123 leads; a long digit tail does not.
#[test]
fn extract_task_terms_caseless_words_are_code_like_only_when_short() {
    let terms = extract_task_terms("alpha A123 zeta");
    assert_eq!(terms.first().map(String::as_str), Some("A123"), "{terms:?}");

    let long_caseless = format!("X{}", "9".repeat(10));
    let terms = extract_task_terms(&format!("alpha {long_caseless} zeta"));
    assert_ne!(
        terms.first().map(String::as_str),
        Some(long_caseless.as_str()),
        "long caseless word must not take the code-like slot: {terms:?}"
    );
}

// ── rank formatting ─────────────────────────────────────────────────────────

#[test]
fn format_rank_bar_counts_are_exact() {
    assert_eq!(format_rank(-1.0), "··········");
    assert_eq!(format_rank(0.0), "··········");
    assert_eq!(format_rank(0.01), "█·········");
    assert_eq!(format_rank(0.5), "█████·····");
    assert_eq!(format_rank(0.95), "█████████·");
    assert_eq!(format_rank(5.0), "██████████");
}

// ── public API boundaries ───────────────────────────────────────────────────

/// Exactly `min_source_files` files is enough: the guard is strict less-than.
#[tokio::test]
async fn generate_repo_map_proceeds_at_exact_minimum() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("one.rs"), "pub fn one() {}").unwrap();
    std::fs::write(dir.path().join("two.rs"), "pub fn two() {}").unwrap();

    let map = generate_repo_map(
        dir.path(),
        &RepoMapOptions {
            min_source_files: 2,
            ..Default::default()
        },
    )
    .await;
    assert!(map.is_some(), "two files meet a minimum of two");
}

/// When the file cap forces selection, a path match outweighs a body match:
/// the multiplier on the relative-path score is real.
#[tokio::test]
async fn select_source_files_path_score_outweighs_body_score() {
    let dir = tempfile::tempdir().unwrap();
    let filler = dir.path().join("crates/filler/src");
    std::fs::create_dir_all(&filler).unwrap();
    for index in 0..MAX_FILES {
        std::fs::write(
            filler.join(format!("file_{index:03}.rs")),
            format!("fn filler_{index}() {{}}"),
        )
        .unwrap();
    }
    let authoritative = dir.path().join("crates/auth/src");
    std::fs::create_dir_all(&authoritative).unwrap();
    std::fs::write(authoritative.join("widget_path.rs"), "pub fn plain() {}").unwrap();
    std::fs::write(
        filler.join("file_000.rs"),
        "fn body() { let p = widget_path; let q = zzbodymark; let r = thirdmark; }",
    )
    .unwrap();

    let selected = select_source_files(
        walk_source_files(dir.path()),
        &[
            "widget_path".into(),
            "zzbodymark".into(),
            "thirdmark".into(),
        ],
    )
    .await;
    let pos_of = |name: &str| {
        selected
            .iter()
            .position(|(p, _)| p.replace('\\', "/") == name)
            .unwrap_or_else(|| panic!("{name} not selected"))
    };
    assert!(
        pos_of("crates/auth/src/widget_path.rs") < pos_of("crates/filler/src/file_000.rs"),
        "path match (x4 of one term) outranks three body-term hits: {selected:?}"
    );
}

// ── context-map rendering ───────────────────────────────────────────────────

fn ranked_def(file: &str, name: &str, rank: f64, line: usize, snippet: &str) -> RankedDef {
    RankedDef {
        file: file.to_string(),
        name: name.to_string(),
        rank,
        line,
        snippet: snippet.to_string(),
    }
}

#[test]
fn context_map_routes_by_terms_and_budget() {
    let ranked = vec![ranked_def(
        "src/gizmo.rs",
        "needle_fn",
        0.5,
        4,
        "fn needle_fn() {}",
    )];

    // Terms plus a real budget -> evidence section.
    let out = render_context_map(&ranked, &["needle".to_string()], 1024);
    assert!(out.contains("Task evidence"), "{out}");

    // Exactly 256 tokens still takes the evidence path (strict <).
    let out = render_context_map(&ranked, &["needle".to_string()], 256);
    assert!(
        out.contains("Task evidence"),
        "boundary budget stays evidential: {out}"
    );

    // Under the floor -> plain map even with terms.
    let out = render_context_map(&ranked, &["needle".to_string()], 255);
    assert!(!out.contains("Task evidence"), "{out}");

    // No terms -> plain map.
    let out = render_context_map(&ranked, &[], 1024);
    assert!(!out.contains("Task evidence"), "{out}");
}

/// Evidence lines consume budget additively and an exactly-fitting entry
/// renders: the overflow guard is strict inequality on the running estimate.
#[test]
fn task_evidence_budget_is_additive_and_inclusive_at_exact_fit() {
    let header = "Task evidence (task-linked definitions before global rank):";
    let file_line = "a.rs";
    let def_line = format!("  L{:>4}  {}", 7, "fn one_two_three() {}");
    let header_est = estimate_tokens(header, 3.5);
    let file_est = estimate_tokens(file_line, 3.5);
    let def_est = estimate_tokens(&def_line, 3.5);

    // Exactly enough room for header + file + one definition.
    let defs = [ranked_def(
        "a.rs",
        "one_fn",
        0.9,
        7,
        "fn one_two_three() {}",
    )];
    let evidence: Vec<(u32, &RankedDef)> = defs.iter().map(|d| (9u32, d)).collect();

    let out = render_task_evidence(&evidence, header_est + file_est + def_est);
    assert!(
        out.contains("one_two_three"),
        "exact fit still renders: {out:?}"
    );
    assert!(
        !out.contains("truncated"),
        "exact fit does not truncate: {out:?}"
    );

    // Room for the definition alone: the file header cannot fit too.
    let out = render_task_evidence(&evidence, header_est + def_est);
    assert!(
        out.contains("truncated"),
        "file line cannot ride along for free: {out:?}"
    );
    assert!(
        !out.contains("one_two_three"),
        "nothing renders when the first entry already busts the budget: {out:?}"
    );

    // Two small definitions fit under a generous budget.
    let defs = [
        ranked_def("a.rs", "one_fn", 0.9, 7, "fn alpha_one() {}"),
        ranked_def("a.rs", "two_fn", 0.5, 8, "fn beta_two() {}"),
    ];
    let evidence: Vec<(u32, &RankedDef)> = defs
        .iter()
        .enumerate()
        .map(|(i, d)| ((9 - i as u32), d))
        .collect();
    let out = render_task_evidence(&evidence, (header_est + file_est + 2 * def_est) * 4);
    assert!(
        out.contains("alpha_one") && out.contains("beta_two"),
        "{out:?}"
    );
}

/// The omission line reports the true remainder: lower-ranked leftover
/// definitions in the current file plus three per untouched file.
#[test]
fn repo_map_truncation_message_counts_are_exact() {
    fn def_line(rank: f64, line: usize, snippet: &str) -> String {
        format!("  {:>10} L{:>4}  {}", format_rank(rank), line, snippet)
    }
    let header_est = estimate_tokens("Repo map (0.0k tokens):", 3.5);
    let blank_est = estimate_tokens("", 3.5);
    let f_line_est = estimate_tokens("f.rs", 3.5);
    let keep_a_est = estimate_tokens(&def_line(0.80, 1, "fn keep_a() {}"), 3.5);
    let budget = header_est + blank_est + f_line_est + keep_a_est;

    let ranked = vec![
        ranked_def("f.rs", "keep_a", 0.80, 1, "fn keep_a() {}"),
        ranked_def("f.rs", "drop_b", 0.80, 2, "fn drop_b() {}"),
        ranked_def("f.rs", "drop_c", 0.70, 3, "fn drop_c() {}"),
        ranked_def("g.rs", "gone_d", 0.60, 1, "fn gone_d() {}"),
    ];

    let out = render_repo_map(&ranked, budget);
    assert!(
        !out.contains("drop_b"),
        "second def exceeds budget: {out:?}"
    );
    // drop_b overflows: leftovers within f.rs below its rank = drop_c (1),
    // plus three per unstarted file (g.rs) -> 1 + 2*3 = 7.
    let expected = "  ... (+7 more symbols across 2 files omitted)";
    assert!(
        out.contains(expected),
        "omission arithmetic pinned: wanted '{expected}' in:\n{out}"
    );
    assert!(out.contains("(repo map truncated"));
}

/// An exactly-spent budget still admits the next file's header line: the
/// outer guard is strict greater-than.
#[test]
fn repo_map_outer_budget_guard_is_strict() {
    let ranked = vec![
        ranked_def("f.rs", "only_a", 0.9, 1, "fn only_a() {}"),
        ranked_def("g.rs", "only_b", 0.8, 1, "fn only_b() {}"),
    ];
    let header_and_blank =
        estimate_tokens("Repo map (0.1k tokens):", 3.5) + estimate_tokens("", 3.5);
    let f_line = estimate_tokens("f.rs", 3.5);
    let only_a = estimate_tokens(
        &format!("  {:>10} L{:>4}  {}", format_rank(0.9), 1, "fn only_a() {}"),
        3.5,
    );
    let budget = header_and_blank + f_line + only_a;

    let out = render_repo_map(&ranked, budget);
    assert!(
        out.contains("g.rs"),
        "an exactly-spent budget still opens the next file:\n{out}"
    );
}

// ── file walking ────────────────────────────────────────────────────────────

/// The walk reaches eight directory levels but stops before nine-deep files,
/// skips vendored/generated directories and dotfiles, and enforces the file
/// size ceiling strictly.
#[test]
fn walk_depth_skips_and_size_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let mut deep = root.to_path_buf();
    for level in 1..=9 {
        deep = deep.join(format!("d{level}"));
    }
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("too_deep.rs"), "fn too_deep() {}").unwrap();

    let mut shallow8 = root.to_path_buf();
    for level in 1..=8 {
        shallow8 = shallow8.join(format!("d{level}"));
    }
    std::fs::create_dir_all(&shallow8).unwrap();
    std::fs::write(shallow8.join("reachable.rs"), "fn reachable() {}").unwrap();

    for (rel, content) in [
        (".hidden_dir/hidden.rs", "fn hidden() {}"),
        ("target/artifact.rs", "fn artifact() {}"),
        ("node_modules/vendor.js", "function vendor() {}"),
        ("__pycache__/cached.py", "def cached(): pass"),
        (".dotfile.rs", "fn dotfile() {}"),
    ] {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    let oversized = root.join("oversized.rs");
    std::fs::write(&oversized, "x".repeat(MAX_FILE_SIZE)).unwrap();

    let found: Vec<String> = walk_source_files(root)
        .into_iter()
        .map(|(rel, _)| rel.replace('\\', "/"))
        .collect();

    assert!(
        found.iter().any(|f| f.ends_with("d8/reachable.rs")),
        "level-eight files are within reach: {found:?}"
    );
    assert!(
        !found.iter().any(|f| f.ends_with("d9/too_deep.rs")),
        "level-nine files are beyond the walk: {found:?}"
    );
    for skipped in [
        "hidden",
        "artifact",
        "vendor",
        "cached",
        "dotfile",
        "oversized",
    ] {
        assert!(
            !found.iter().any(|f| f.contains(skipped)),
            "{skipped} must be skipped: {found:?}"
        );
    }
}

#[test]
fn probe_full_ts_query() {
    let (_n, lang) = detect_lang("a.ts").unwrap();
    let q = Query::new(&lang, query_source("typescript"));
    eprintln!("full ts query ok: {}", q.is_ok());
    if let Err(e) = &q {
        eprintln!("err: {e}");
    }
    let tags = extract_tags("a.ts", "function hello(): void {}", "typescript", &lang);
    eprintln!(
        "tags: {:?}",
        tags.iter()
            .map(|t| (t.name.as_str(), t.is_def))
            .collect::<Vec<_>>()
    );
}
