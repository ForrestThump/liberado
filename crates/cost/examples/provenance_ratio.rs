//! How much of a delegated answer came from the delegation?
//!
//! `delegated-work-is-discarded-at-the-seam.md` was found by hand: one turn where a **504-char**
//! delegate tool result became a **7,872-char** answer citing sources whose content nothing in the
//! pipeline had read. The answer was accurate and well-structured. Every output-quality check would
//! have passed it. What was false was the *provenance*.
//!
//! That failure has a shape you can compute: the ratio between what the face agent **received** from
//! a delegation and what it then **wrote**. A high ratio means the model supplied the specifics
//! itself. This walks the session logs the daemon already writes and reports that ratio per
//! delegation — no inference, no grader, no harness.
//!
//! Run: `cargo run -p liberado-cost --example provenance_ratio -- <data-dir> [ratio-threshold]`
//!
//! **It flags, it does not judge.** A high ratio is sometimes legitimate — a one-line calendar
//! lookup expanded into a readable sentence is fine. It marks transcripts worth reading, which is
//! the cheapest useful thing an "eval" can do before you have a free oracle to grade against.
//!
//! An example rather than a test: it reports on whatever logs it is pointed at, so there is no fixed
//! expected output to assert.

use std::path::Path;

/// Flag a delegation whose answer is this many times longer than the material it was given.
const DEFAULT_RATIO_FLAG: f64 = 3.0;

/// A delegate tool result is wrapped in framing the face never treats as content. Measuring it as
/// material would understate the ratio — on the 504-char case roughly a third of the payload was
/// the session id and dispatch-journal path.
fn substance(content: &str) -> usize {
    content
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty()
                && !t.starts_with("[session:")
                && !t.starts_with("[dispatch journal:")
                && !t.starts_with("RESULT (")
        })
        .map(|l| l.trim().len())
        .sum()
}

/// A delegation that **failed** hands the face no material by construction, so the ratio says
/// nothing about provenance — the face is recovering, not inventing. Found by reading a flagged
/// transcript: `RESULT (Failed): blocked …` followed by *"Let me try that with a more structured
/// framing."*, which is correct behaviour scoring 12.4×.
fn succeeded(content: &str) -> bool {
    content.trim_start().starts_with("RESULT (Succeeded)")
}

struct Delegation {
    conversation: String,
    received: usize,
    written: usize,
}

impl Delegation {
    fn ratio(&self) -> f64 {
        if self.received == 0 {
            f64::INFINITY
        } else {
            self.written as f64 / self.received as f64
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let data_dir = args.next().unwrap_or_else(|| ".liberado".into());
    let flag_at: f64 = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_RATIO_FLAG);

    let sessions = Path::new(&data_dir).join("sessions");
    let Ok(entries) = std::fs::read_dir(&sessions) else {
        eprintln!("no sessions directory at {}", sessions.display());
        return;
    };

    let mut found = Vec::new();
    let mut logs = 0usize;
    let mut skipped_failed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        logs += 1;
        let (delegations, failed) = scan_log(&text);
        found.extend(delegations);
        skipped_failed += failed;
    }

    found.sort_by(|a, b| {
        b.ratio()
            .partial_cmp(&a.ratio())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    println!(
        "session logs scanned: {logs}   delegations answered: {}   failed delegations skipped: {skipped_failed}",
        found.len()
    );
    if found.is_empty() {
        println!("no delegation followed by an answer — nothing to report");
        return;
    }

    let flagged: Vec<&Delegation> = found.iter().filter(|d| d.ratio() >= flag_at).collect();
    println!(
        "flagged at ratio >= {flag_at:.1}: {} of {}\n",
        flagged.len(),
        found.len()
    );
    println!(
        "{:<30} {:>9} {:>9} {:>8}",
        "conversation", "received", "written", "ratio"
    );
    for d in found.iter().take(20) {
        let mark = if d.ratio() >= flag_at { " <-" } else { "" };
        println!(
            "{:<30} {:>9} {:>9} {:>8.1}{mark}",
            d.conversation,
            d.received,
            d.written,
            d.ratio()
        );
    }

    let mut ratios: Vec<f64> = found
        .iter()
        .map(Delegation::ratio)
        .filter(|r| r.is_finite())
        .collect();
    ratios.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if !ratios.is_empty() {
        println!("\nmedian ratio {:.1}", ratios[ratios.len() / 2]);
    }
}

/// Walk one session log in order, pairing delegate tool results with the answer that followed.
///
/// Multiple delegations before a single answer are summed: the face agent had all of that material
/// in hand when it wrote.
fn scan_log(text: &str) -> (Vec<Delegation>, usize) {
    let mut out = Vec::new();
    let mut pending_received = 0usize;
    let mut conversation = String::new();
    let mut skipped_failed = 0usize;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("kind").and_then(|k| k.as_str()) != Some("node") {
            continue;
        }
        let author = v.get("author").and_then(|a| a.as_str()).unwrap_or("");
        let content = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("");

        match author {
            // Only successful chat delegations — other tools, and failures, are not this question.
            "tool" if content.contains("chat-delegate-") && !succeeded(content) => {
                skipped_failed += 1;
            }
            "tool" if content.contains("chat-delegate-") => {
                if conversation.is_empty() {
                    conversation = v
                        .get("conversation_id")
                        .and_then(|c| c.as_str())
                        .unwrap_or("?")
                        .to_string();
                }
                pending_received += substance(content);
            }
            "assistant" if pending_received > 0 => {
                out.push(Delegation {
                    conversation: conversation.clone(),
                    received: pending_received,
                    written: content.trim().len(),
                });
                pending_received = 0;
            }
            _ => {}
        }
    }
    (out, skipped_failed)
}
