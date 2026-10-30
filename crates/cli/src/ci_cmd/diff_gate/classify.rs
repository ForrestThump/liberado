//! Pair changed functions with merge-base functions.
//!
//! Exact identity (file + qualified name + occurrence, after the git rename map) wins.
//! Anything left over is paired one-to-one with a base function that no longer exists:
//! same qualified name first, then normalized body similarity. A base function that still
//! exists is not a candidate, so a copy of it stays new.

use std::collections::{BTreeMap, BTreeSet};

use super::BODY_MATCH_MIN;
use super::body::jaccard;
use super::hunks::ParsedDiff;
use super::model::{Class, FnKey, ScoredFn};
use super::spans::intersects;

#[derive(Clone, Debug)]
pub struct Classified {
    pub head_index: usize,
    pub class: Class,
    pub base_index: Option<usize>,
}

struct Indexed<'a> {
    index: usize,
    key: FnKey,
    score: &'a ScoredFn,
}

struct Pair {
    head: usize,
    base: usize,
    preference: u8,
    similarity_millis: u32,
}

pub fn classify(head: &[ScoredFn], base: &[ScoredFn], diff: &ParsedDiff) -> Vec<Classified> {
    let head_rows = index_fns(head);
    let base_rows = index_fns(base);
    let forward = diff.rename_forward();
    let reverse = diff.rename_reverse();
    let head_keys = key_set(&head_rows);
    let base_by_key = key_map(&base_rows);
    let disappeared = disappeared_bases(&base_rows, &forward, &head_keys);
    let mut used_bases = BTreeSet::new();
    let mut matched = Vec::new();
    let mut pending = Vec::new();
    for row in &head_rows {
        if !changed(row, diff) {
            continue;
        }
        if let Some(found) = exact_base(&base_by_key, &map_back(&row.key, &reverse)) {
            used_bases.insert(found);
            if let Some(base_row) = row_by_index(&base_rows, found) {
                matched.push(exact_class(row, base_row, &reverse));
            }
        } else {
            pending.push(row.index);
        }
    }
    let name_hits = assign(&name_pairs(
        &pending,
        &disappeared,
        &base_rows,
        &head_rows,
        &forward,
        &used_bases,
    ));
    take_assignments(
        &mut pending,
        &mut used_bases,
        &mut matched,
        &base_rows,
        &name_hits,
    );
    let body_hits = assign(&body_pairs(
        &pending,
        &disappeared,
        &base_rows,
        &head_rows,
        &used_bases,
    ));
    take_assignments(
        &mut pending,
        &mut used_bases,
        &mut matched,
        &base_rows,
        &body_hits,
    );
    for head_index in pending {
        matched.push(Classified {
            head_index,
            class: Class::New,
            base_index: None,
        });
    }
    matched
}

fn exact_class(
    row: &Indexed<'_>,
    base: &Indexed<'_>,
    reverse: &BTreeMap<String, String>,
) -> Classified {
    let class = if reverse.contains_key(&row.score.file) {
        Class::RenamedFrom {
            file: base.score.file.clone(),
            function: base.score.function.clone(),
        }
    } else {
        Class::Legacy
    };
    Classified {
        head_index: row.index,
        class,
        base_index: Some(base.index),
    }
}

fn changed(row: &Indexed<'_>, diff: &ParsedDiff) -> bool {
    let lines = diff.lines_for(&row.score.file);
    intersects(row.score.line, row.score.end_line, &lines)
}

fn index_fns(functions: &[ScoredFn]) -> Vec<Indexed<'_>> {
    let mut order: Vec<usize> = (0..functions.len()).collect();
    order.sort_by(|&left, &right| {
        (
            &functions[left].file,
            &functions[left].function,
            functions[left].line,
        )
            .cmp(&(
                &functions[right].file,
                &functions[right].function,
                functions[right].line,
            ))
    });
    let mut seen: BTreeMap<(String, String), u32> = BTreeMap::new();
    order
        .into_iter()
        .map(|index| {
            let score = &functions[index];
            let occurrence = seen
                .entry((score.file.clone(), score.function.clone()))
                .or_insert(0);
            let key = FnKey {
                file: score.file.clone(),
                function: score.function.clone(),
                occurrence: *occurrence,
            };
            *occurrence += 1;
            Indexed { index, key, score }
        })
        .collect()
}

fn key_set(rows: &[Indexed<'_>]) -> BTreeSet<FnKey> {
    rows.iter().map(|row| row.key.clone()).collect()
}

fn key_map(rows: &[Indexed<'_>]) -> BTreeMap<FnKey, usize> {
    rows.iter()
        .map(|row| (row.key.clone(), row.index))
        .collect()
}

fn disappeared_bases(
    base_rows: &[Indexed<'_>],
    forward: &BTreeMap<String, String>,
    head_keys: &BTreeSet<FnKey>,
) -> BTreeSet<usize> {
    base_rows
        .iter()
        .filter(|row| !head_keys.contains(&map_forward(&row.key, forward)))
        .map(|row| row.index)
        .collect()
}

fn map_back(key: &FnKey, reverse: &BTreeMap<String, String>) -> FnKey {
    FnKey {
        file: reverse
            .get(&key.file)
            .cloned()
            .unwrap_or_else(|| key.file.clone()),
        function: key.function.clone(),
        occurrence: key.occurrence,
    }
}

fn map_forward(key: &FnKey, forward: &BTreeMap<String, String>) -> FnKey {
    FnKey {
        file: forward
            .get(&key.file)
            .cloned()
            .unwrap_or_else(|| key.file.clone()),
        function: key.function.clone(),
        occurrence: key.occurrence,
    }
}

fn exact_base(base_by_key: &BTreeMap<FnKey, usize>, key: &FnKey) -> Option<usize> {
    base_by_key.get(key).copied()
}

fn name_pairs(
    pending: &[usize],
    disappeared: &BTreeSet<usize>,
    base_rows: &[Indexed<'_>],
    head_rows: &[Indexed<'_>],
    forward: &BTreeMap<String, String>,
    used: &BTreeSet<usize>,
) -> Vec<Pair> {
    let mut pairs = Vec::new();
    for &head_index in pending {
        let Some(head) = row_by_index(head_rows, head_index) else {
            continue;
        };
        for base in base_rows {
            if !candidate(base.index, disappeared, used)
                || head.score.function != base.score.function
            {
                continue;
            }
            pairs.push(pair_for(head, base, forward));
        }
    }
    pairs
}

fn body_pairs(
    pending: &[usize],
    disappeared: &BTreeSet<usize>,
    base_rows: &[Indexed<'_>],
    head_rows: &[Indexed<'_>],
    used: &BTreeSet<usize>,
) -> Vec<Pair> {
    let mut pairs = Vec::new();
    for &head_index in pending {
        let Some(head) = row_by_index(head_rows, head_index) else {
            continue;
        };
        for base in base_rows {
            if !candidate(base.index, disappeared, used) {
                continue;
            }
            let similarity = jaccard(
                &head.score.body,
                &head.score.function,
                &base.score.body,
                &base.score.function,
            );
            if similarity + f64::EPSILON < BODY_MATCH_MIN {
                continue;
            }
            pairs.push(Pair {
                head: head.index,
                base: base.index,
                preference: 0,
                similarity_millis: millis(similarity),
            });
        }
    }
    pairs
}

fn pair_for(head: &Indexed<'_>, base: &Indexed<'_>, forward: &BTreeMap<String, String>) -> Pair {
    let preference = if forward.get(&base.score.file) == Some(&head.score.file) {
        2
    } else {
        1
    };
    let similarity = jaccard(
        &head.score.body,
        &head.score.function,
        &base.score.body,
        &base.score.function,
    );
    Pair {
        head: head.index,
        base: base.index,
        preference,
        similarity_millis: millis(similarity),
    }
}

fn candidate(index: usize, disappeared: &BTreeSet<usize>, used: &BTreeSet<usize>) -> bool {
    disappeared.contains(&index) && !used.contains(&index)
}

fn millis(similarity: f64) -> u32 {
    (similarity.clamp(0.0, 1.0) * 1000.0).round() as u32
}

fn assign(pairs: &[Pair]) -> Vec<(usize, usize)> {
    let mut ordered: Vec<&Pair> = pairs.iter().collect();
    ordered.sort_by(|left, right| {
        right
            .preference
            .cmp(&left.preference)
            .then(right.similarity_millis.cmp(&left.similarity_millis))
            .then(left.head.cmp(&right.head))
            .then(left.base.cmp(&right.base))
    });
    let mut used_head = BTreeSet::new();
    let mut used_base = BTreeSet::new();
    let mut chosen = Vec::new();
    for pair in ordered {
        if used_head.contains(&pair.head) || used_base.contains(&pair.base) {
            continue;
        }
        used_head.insert(pair.head);
        used_base.insert(pair.base);
        chosen.push((pair.head, pair.base));
    }
    chosen
}

fn take_assignments(
    pending: &mut Vec<usize>,
    used_bases: &mut BTreeSet<usize>,
    matched: &mut Vec<Classified>,
    base_rows: &[Indexed<'_>],
    hits: &[(usize, usize)],
) {
    for &(head_index, base_index) in hits {
        pending.retain(|index| *index != head_index);
        let Some(base) = row_by_index(base_rows, base_index) else {
            continue;
        };
        used_bases.insert(base_index);
        matched.push(Classified {
            head_index,
            class: Class::RenamedFrom {
                file: base.score.file.clone(),
                function: base.score.function.clone(),
            },
            base_index: Some(base_index),
        });
    }
}

fn row_by_index<'a>(rows: &'a [Indexed<'a>], index: usize) -> Option<&'a Indexed<'a>> {
    rows.iter().find(|row| row.index == index)
}

#[cfg(test)]
mod tests {
    use super::classify;
    use crate::ci_cmd::diff_gate::hunks::ParsedDiff;
    use crate::ci_cmd::diff_gate::hunks::{FileChange, parse_unified_diff};
    use crate::ci_cmd::diff_gate::model::{Class, ScoredFn};

    fn scored(file: &str, function: &str, line: u32, end: u32, cyclo: f64, body: &str) -> ScoredFn {
        ScoredFn {
            file: file.into(),
            function: function.into(),
            line,
            end_line: end,
            cyclomatic: cyclo,
            crap: Some(cyclo),
            body: body.into(),
        }
    }

    fn diff_touch(path: &str, lines: &[u32]) -> ParsedDiff {
        ParsedDiff {
            files: vec![FileChange {
                old_path: Some(path.into()),
                new_path: Some(path.into()),
                touched_lines: lines.iter().copied().collect(),
                deleted: false,
                is_rename: false,
            }],
        }
    }

    #[test]
    fn an_untouched_function_is_not_classified() {
        let head = vec![scored("a.rs", "keep", 1, 3, 2.0, "fn keep() { 1 }")];
        let base = head.clone();
        let diff = diff_touch("a.rs", &[20]);
        assert!(classify(&head, &base, &diff).is_empty());
    }

    #[test]
    fn a_new_function_has_no_base_match() {
        let head = vec![scored("a.rs", "fresh", 4, 6, 1.0, "fn fresh() { 1 }")];
        let found = classify(&head, &[], &diff_touch("a.rs", &[5]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].class, Class::New);
        assert!(found[0].base_index.is_none());
    }

    #[test]
    fn a_touched_function_on_the_base_is_legacy() {
        let body = "fn work() { 1 }";
        let head = vec![scored("a.rs", "work", 1, 4, 4.0, body)];
        let base = vec![scored("a.rs", "work", 1, 3, 3.0, body)];
        let found = classify(&head, &base, &diff_touch("a.rs", &[2]));
        assert_eq!(found[0].class, Class::Legacy);
        assert_eq!(found[0].base_index, Some(0));
    }

    #[test]
    fn a_renamed_file_keeps_the_base_identity() {
        let diff = parse_unified_diff(
            "\
diff --git a/old.rs b/new.rs
rename from old.rs
rename to new.rs
--- a/old.rs
+++ b/new.rs
@@ -1 +1 @@
-fn work() { 1 }
+fn work() { 1 + 1 }
",
        );
        let head = vec![scored("new.rs", "work", 1, 1, 2.0, "fn work() { 1 + 1 }")];
        let base = vec![scored("old.rs", "work", 1, 1, 1.0, "fn work() { 1 }")];
        let found = classify(&head, &base, &diff);
        match &found[0].class {
            Class::RenamedFrom { file, function } => {
                assert_eq!(file, "old.rs");
                assert_eq!(function, "work");
            }
            other => panic!("expected a rename match, got {other:?}"),
        }
    }

    #[test]
    fn a_function_moved_to_another_file_matches_by_name() {
        let diff = parse_unified_diff(
            "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1 +0,0 @@
-fn moved() { alpha beta gamma }
diff --git a/b.rs b/b.rs
--- a/b.rs
+++ b/b.rs
@@ -1,0 +2 @@
+fn moved() { alpha beta gamma }
",
        );
        let head = vec![
            scored("a.rs", "stay", 1, 2, 1.0, "fn stay() { 1 }"),
            scored(
                "b.rs",
                "moved",
                2,
                2,
                8.0,
                "fn moved() { alpha beta gamma }",
            ),
        ];
        let base = vec![
            scored("a.rs", "stay", 1, 2, 1.0, "fn stay() { 1 }"),
            scored(
                "a.rs",
                "moved",
                3,
                3,
                8.0,
                "fn moved() { alpha beta gamma }",
            ),
        ];
        let found = classify(&head, &base, &diff);
        let moved = found.iter().find(|item| item.head_index == 1).unwrap();
        assert!(matches!(moved.class, Class::RenamedFrom { .. }));
    }

    #[test]
    fn a_renamed_function_matches_on_a_similar_body() {
        let diff = diff_touch("a.rs", &[1]);
        let head = vec![scored(
            "a.rs",
            "after",
            1,
            3,
            4.0,
            "fn after() { alpha beta gamma delta epsilon }",
        )];
        let base = vec![scored(
            "a.rs",
            "before",
            1,
            3,
            4.0,
            "fn before() { alpha beta gamma delta epsilon }",
        )];
        let found = classify(&head, &base, &diff);
        match &found[0].class {
            Class::RenamedFrom { function, .. } => assert_eq!(function, "before"),
            other => panic!("expected a body match, got {other:?}"),
        }
    }

    #[test]
    fn a_copy_of_a_function_that_still_exists_is_new() {
        let diff = parse_unified_diff(
            "\
diff --git a/b.rs b/b.rs
new file mode 100644
--- /dev/null
+++ b/b.rs
@@ -0,0 +1 @@
+fn over() { alpha beta gamma delta epsilon }
",
        );
        let body = "fn over() { alpha beta gamma delta epsilon }";
        let head = vec![
            scored("a.rs", "over", 1, 4, 30.0, body),
            scored("b.rs", "over", 1, 1, 30.0, body),
        ];
        let base = vec![scored("a.rs", "over", 1, 4, 30.0, body)];
        let found = classify(&head, &base, &diff);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].head_index, 1);
        assert_eq!(found[0].class, Class::New);
    }

    #[test]
    fn a_split_keeps_the_original_legacy_and_the_extract_new() {
        let diff = diff_touch("a.rs", &[2, 8]);
        let original = "fn big() { alpha beta gamma delta epsilon zeta }";
        let head = vec![
            scored("a.rs", "big", 1, 5, 21.0, original),
            scored("a.rs", "piece", 7, 10, 4.0, "fn piece() { only here }"),
        ];
        let base = vec![scored("a.rs", "big", 1, 12, 21.0, original)];
        let found = classify(&head, &base, &diff);
        let big = found.iter().find(|item| item.head_index == 0).unwrap();
        let piece = found.iter().find(|item| item.head_index == 1).unwrap();
        assert_eq!(big.class, Class::Legacy);
        assert_eq!(piece.class, Class::New);
    }

    #[test]
    fn a_dissimilar_replacement_does_not_inherit_the_removed_function() {
        let diff = diff_touch("a.rs", &[1]);
        let head = vec![scored(
            "a.rs",
            "fresh",
            1,
            3,
            30.0,
            "fn fresh() { one two three four }",
        )];
        let base = vec![scored(
            "a.rs",
            "old",
            1,
            3,
            30.0,
            "fn old() { alpha beta gamma delta epsilon zeta }",
        )];
        let found = classify(&head, &base, &diff);
        assert_eq!(found[0].class, Class::New);
    }
}
