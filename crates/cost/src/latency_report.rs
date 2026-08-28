use std::collections::BTreeMap;

use serde::Serialize;

use crate::JournalEvent;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LatencySummary {
    pub role: String,
    pub calls: usize,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub max_ms: u64,
    pub ttft_p50_ms: Option<u64>,
    pub total_tokens: u64,
}

pub fn latency_summary(events: &[JournalEvent]) -> Vec<LatencySummary> {
    let mut grouped: BTreeMap<&str, Vec<&JournalEvent>> = BTreeMap::new();
    for event in events.iter().filter(|event| event.kind == "llm_call") {
        grouped.entry(&event.role).or_default().push(event);
    }
    grouped
        .into_iter()
        .map(|(role, rows)| summarize_role(role, &rows))
        .collect()
}

fn summarize_role(role: &str, rows: &[&JournalEvent]) -> LatencySummary {
    let mut wall: Vec<u64> = rows.iter().map(|event| event.wall_ms).collect();
    wall.sort_unstable();
    let mut ttft: Vec<u64> = rows.iter().filter_map(|event| event.ttft_ms).collect();
    ttft.sort_unstable();
    LatencySummary {
        role: role.into(),
        calls: rows.len(),
        p50_ms: percentile(&wall, 50),
        p95_ms: percentile(&wall, 95),
        max_ms: wall.last().copied().unwrap_or(0),
        ttft_p50_ms: (!ttft.is_empty()).then(|| percentile(&ttft, 50)),
        total_tokens: rows
            .iter()
            .map(|event| u64::from(event.total_tokens.unwrap_or(0)))
            .sum(),
    }
}

fn percentile(sorted: &[u64], percentage: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[(sorted.len() - 1) * percentage / 100]
}

pub fn format_latency_report(rows: &[LatencySummary]) -> String {
    if rows.is_empty() {
        return "no llm_call records yet\n".into();
    }
    let mut output = format!(
        "{:<18} {:>7} {:>10} {:>10} {:>10} {:>12} {:>12}\n",
        "role", "calls", "p50_ms", "p95_ms", "max_ms", "ttft_p50", "tokens"
    );
    for row in rows {
        let ttft = row
            .ttft_p50_ms
            .map_or_else(|| "-".into(), |value| value.to_string());
        output.push_str(&format!(
            "{:<18} {:>7} {:>10} {:>10} {:>10} {:>12} {:>12}\n",
            row.role, row.calls, row.p50_ms, row.p95_ms, row.max_ms, ttft, row.total_tokens
        ));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(role: &str, wall_ms: u64, ttft_ms: Option<u64>, tokens: u32) -> JournalEvent {
        JournalEvent {
            ts_ms: 1,
            correlation: "c".into(),
            role: role.into(),
            model: "m".into(),
            kind: "llm_call".into(),
            wall_ms,
            ttft_ms,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: Some(tokens),
            cached_prompt_tokens: None,
            finish: "stop".into(),
            tool_calls: 0,
            streamed: true,
            repeat_calls: None,
        }
    }

    #[test]
    fn summary_matches_the_shell_report_percentile_contract() {
        let events = [
            event("face", 10, Some(2), 3),
            event("face", 20, Some(4), 5),
            event("face", 30, None, 7),
            event("face", 40, Some(8), 11),
            event("face", 50, Some(10), 13),
        ];
        let rows = latency_summary(&events);
        assert_eq!(
            rows,
            vec![LatencySummary {
                role: "face".into(),
                calls: 5,
                p50_ms: 30,
                p95_ms: 40,
                max_ms: 50,
                ttft_p50_ms: Some(4),
                total_tokens: 39,
            }]
        );
    }

    #[test]
    fn empty_report_is_explicit() {
        assert_eq!(format_latency_report(&[]), "no llm_call records yet\n");
    }
}
