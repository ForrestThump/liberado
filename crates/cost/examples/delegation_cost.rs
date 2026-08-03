//! One-off measurement behind the round-2 D2 question: what does delegating cost the turns that
//! follow it?
//!
//! `delegated-work-is-discarded-at-the-seam.md` left the context cost of `relay_directive`
//! unresolved. Turn grouping makes it answerable from the journal the daemon already writes: a turn
//! "delegated" when child-dispatch hops were folded into it, so the turn *after* one of those is
//! where a relayed report would show up as extra prompt.
//!
//! Reported in tokens rather than dollars — an unpriced deployment (no `[[models]]` rates) still
//! answers this question, and the shape of the answer does not depend on the rate card.
//!
//! Run: `cargo run -p liberado-cost --example delegation_cost -- <data-dir>`
//!
//! An example rather than a test: it reports on whatever journal it is pointed at, so there is no
//! fixed expected output to assert.

use std::collections::HashMap;
use std::path::Path;

use liberado_cost::{
    JournalEvent, load_dispatch_parent_map, load_latency_events, root_conversation,
};

/// One grouped turn — the same boundary rule `TurnGrowth` applies, keeping the extra bit of
/// bookkeeping (did child work land inside it?) that the report type has no reason to carry.
struct Turn {
    delegated: bool,
    prompt: Option<u32>,
    cached: Option<u32>,
}

fn main() {
    let data_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".liberado".into());
    let data = Path::new(&data_dir);
    let events = load_latency_events(&data.join("latency").join("events.jsonl")).expect("journal");
    let parents = load_dispatch_parent_map(&data.join("dispatches")).unwrap_or_default();

    let mut indexed: Vec<(String, &JournalEvent)> = events
        .iter()
        .map(|e| (root_conversation(&e.correlation, &parents), e))
        .collect();
    indexed.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.ts_ms.cmp(&b.1.ts_ms)));

    let mut by_conv: HashMap<String, Vec<Turn>> = HashMap::new();
    let mut i = 0;
    while i < indexed.len() {
        let conv = indexed[i].0.clone();
        let start = i;
        while i < indexed.len() && indexed[i].0 == conv {
            i += 1;
        }
        let slice = &indexed[start..i];
        let has_own = slice.iter().any(|(_, e)| e.correlation == conv);
        let mut turns = Vec::new();
        let mut cur: Option<Turn> = None;
        for (_, e) in slice {
            let boundary = !has_own || e.correlation == conv;
            let t = cur.get_or_insert(Turn {
                delegated: false,
                prompt: None,
                cached: None,
            });
            if e.correlation != conv {
                t.delegated = true;
            }
            if boundary {
                t.prompt = e.prompt_tokens;
                t.cached = e.cached_prompt_tokens;
            }
            if boundary && (e.tool_calls == 0 || e.finish == "error") {
                turns.push(cur.take().expect("just inserted"));
            }
        }
        if let Some(t) = cur {
            turns.push(t);
        }
        by_conv.insert(conv, turns);
    }

    let mut after_delegating: Vec<(u32, Option<u32>)> = Vec::new();
    let mut after_plain: Vec<(u32, Option<u32>)> = Vec::new();
    for turns in by_conv.values() {
        for pair in turns.windows(2) {
            let Some(prompt) = pair[1].prompt else {
                continue;
            };
            if pair[0].delegated {
                after_delegating.push((prompt, pair[1].cached));
            } else {
                after_plain.push((prompt, pair[1].cached));
            }
        }
    }

    let delegating: usize = by_conv.values().flatten().filter(|t| t.delegated).count();
    let all: usize = by_conv.values().map(Vec::len).sum();
    println!(
        "conversations={}  turns={all}  delegating_turns={delegating}",
        by_conv.len()
    );
    summarize("turn AFTER a delegating turn    ", &after_delegating);
    summarize("turn AFTER a non-delegating turn", &after_plain);
}

fn summarize(label: &str, rows: &[(u32, Option<u32>)]) {
    if rows.is_empty() {
        println!("{label}: no samples");
        return;
    }
    let n = rows.len();
    let mean = rows.iter().map(|(p, _)| u64::from(*p)).sum::<u64>() as f64 / n as f64;
    let mut sorted: Vec<u32> = rows.iter().map(|(p, _)| *p).collect();
    sorted.sort_unstable();
    let median = sorted[n / 2];
    // Cache rate over the rows that reported cached tokens at all — absent is not zero.
    let reported: Vec<&(u32, Option<u32>)> = rows.iter().filter(|(_, c)| c.is_some()).collect();
    let hit = if reported.is_empty() {
        "n/a".to_string()
    } else {
        let cached: u64 = reported.iter().map(|(_, c)| u64::from(c.unwrap())).sum();
        let prompt: u64 = reported.iter().map(|(p, _)| u64::from(*p)).sum();
        format!("{:.1}%", cached as f64 / prompt as f64 * 100.0)
    };
    println!("{label}: n={n}  mean_prompt={mean:.0}  median_prompt={median}  cache_hit={hit}");
}
