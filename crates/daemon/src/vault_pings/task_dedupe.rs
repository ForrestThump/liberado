//! Collapse copied open tasks before rank and `task_limit`.
//!
//! Evening debriefs and weekly reviews paste the same `- [ ]` line into
//! `Briefs/` and `Journal/`. Those copies are one task, not many.
//!
//! Two open lines are the same item when either:
//! 1. Their normalized descriptions are equal, or
//! 2. One line names the other line's note and the descriptions are a near copy.
//!
//! A line names a note with a trailing-style path backref `*(path)*` or a wiki
//! link `[[path]]` (alias and `#heading` are ignored). Matching ignores `.md`
//! and ASCII case, and accepts the note's file name when the link has no folder.
//! Two copies that only share a backref do not match each other through that
//! link. Different tasks in one note stay separate.
//!
//! A near copy shares at least 4 words and at least half of the combined word
//! set (Jaccard ≥ 1/2). Words come from the normalized description.
//!
//! Normalization, in order:
//! - drop `*(...)*` and `[[...]]` backrefs
//! - drop `#tags`
//! - drop Tasks emoji, and the date, clock, or recurrence that follows 📅 ⏳ 🛫 🔁 ✅ ❌ ➕
//! - drop `*` emphasis and em dashes
//! - drop a prose `due <date>` (`due Oct 22`, `due 2026-10-22`, `due 10/22`)
//! - drop leftover ISO dates and clocks
//! - collapse whitespace and compare in lowercase
//!
//! The kept line is one real occurrence. Prefer the note other copies point at.
//! Otherwise prefer `Life/` or `Tasks/`. `Briefs/` and `Journal/` lose.
//! Priority and due are whatever that kept line already parsed. Rank and
//! `task_limit` run after this.

use std::collections::BTreeSet;

use super::OpenTask;

#[path = "task_norm.rs"]
mod norm;

pub(super) fn note_backrefs(line: &str) -> Vec<String> {
    norm::note_backrefs(line)
}

pub(super) fn collapse_copies(tasks: Vec<OpenTask>) -> Vec<OpenTask> {
    let mut norms = Vec::with_capacity(tasks.len());
    for task in &tasks {
        norms.push(norm::normalize(&task.text));
    }
    let mut parent: Vec<usize> = (0..tasks.len()).collect();
    for left in 0..tasks.len() {
        for right in (left + 1)..tasks.len() {
            if same_item(&tasks[left], &tasks[right], &norms[left], &norms[right]) {
                join(&mut parent, left, right);
            }
        }
    }
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut slot: Vec<Option<usize>> = vec![None; tasks.len()];
    for index in 0..tasks.len() {
        let root = find(&parent, index);
        if let Some(at) = slot[root] {
            groups[at].push(index);
        } else {
            slot[root] = Some(groups.len());
            groups.push(vec![index]);
        }
    }
    let mut kept = Vec::with_capacity(groups.len());
    for group in &groups {
        kept.push(pick(&tasks, group));
    }
    kept
}

fn same_item(left: &OpenTask, right: &OpenTask, left_norm: &str, right_norm: &str) -> bool {
    if same_description(left_norm, right_norm) {
        return true;
    }
    linked(left, right) && near_copy(left_norm, right_norm)
}

fn same_description(left: &str, right: &str) -> bool {
    !left.is_empty() && left == right
}

fn linked(left: &OpenTask, right: &OpenTask) -> bool {
    backref_hits(&left.backrefs, &right.path) || backref_hits(&right.backrefs, &left.path)
}

fn backref_hits(backrefs: &[String], path: &str) -> bool {
    for backref in backrefs {
        if paths_match(backref, path) {
            return true;
        }
    }
    false
}

fn paths_match(backref: &str, path: &str) -> bool {
    let link = norm_note(backref);
    let note = norm_note(path);
    if link.is_empty() || note.is_empty() {
        return false;
    }
    if link == note {
        return true;
    }
    if let Some(stem) = note.rsplit('/').next()
        && link == stem
    {
        return true;
    }
    link.contains('/') && (note.ends_with(&link) || link.ends_with(&note))
}

fn norm_note(value: &str) -> String {
    let trimmed = value.trim().trim_matches('*').trim();
    let slash = trimmed.replace('\\', "/");
    let no_dot = slash.strip_prefix("./").unwrap_or(&slash);
    let folded = no_dot.to_ascii_lowercase();
    folded.strip_suffix(".md").unwrap_or(&folded).to_string()
}

fn near_copy(left: &str, right: &str) -> bool {
    let left_words = token_set(left);
    let right_words = token_set(right);
    let mut shared = 0;
    for word in &left_words {
        if right_words.contains(word) {
            shared += 1;
        }
    }
    let union = left_words.len() + right_words.len() - shared;
    shared >= 4 && union > 0 && shared * 2 >= union
}

fn token_set(text: &str) -> BTreeSet<String> {
    let mut words = BTreeSet::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            words.insert(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.insert(current);
    }
    words
}

fn pick(tasks: &[OpenTask], group: &[usize]) -> OpenTask {
    let mut best = group[0];
    for &index in &group[1..] {
        if prefer(&tasks[index], &tasks[best], tasks, group) {
            best = index;
        }
    }
    tasks[best].clone()
}

fn prefer(candidate: &OpenTask, incumbent: &OpenTask, tasks: &[OpenTask], group: &[usize]) -> bool {
    let left = zone_rank(&candidate.path, is_referenced(candidate, tasks, group));
    let right = zone_rank(&incumbent.path, is_referenced(incumbent, tasks, group));
    if left != right {
        return left < right;
    }
    let left_due = candidate.due.is_none();
    let right_due = incumbent.due.is_none();
    if left_due != right_due {
        return !left_due;
    }
    if candidate.priority != incumbent.priority {
        return candidate.priority > incumbent.priority;
    }
    candidate.path < incumbent.path
}

fn is_referenced(task: &OpenTask, tasks: &[OpenTask], group: &[usize]) -> bool {
    for &index in group {
        let other = &tasks[index];
        if other.path == task.path {
            continue;
        }
        if backref_hits(&other.backrefs, &task.path) {
            return true;
        }
    }
    false
}

/// Lower is a better home for the digest line.
fn zone_rank(path: &str, referenced: bool) -> u8 {
    if referenced {
        return 0;
    }
    let segment = path.split('/').next().unwrap_or(path);
    if ascii_eq(segment, "Life") || ascii_eq(segment, "Tasks") {
        return 1;
    }
    if ascii_eq(segment, "Briefs") || ascii_eq(segment, "Journal") {
        return 3;
    }
    2
}

fn ascii_eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn join(parent: &mut [usize], left: usize, right: usize) {
    let left_root = find(parent, left);
    let right_root = find(parent, right);
    if left_root != right_root {
        parent[right_root] = left_root;
    }
}

fn find(parent: &[usize], mut index: usize) -> usize {
    while parent[index] != index {
        index = parent[index];
    }
    index
}
