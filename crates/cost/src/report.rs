//! Human-readable cost report tables.

use crate::rollup::{ConversationRollup, RoleRollup, TurnGrowth, UnpricedLine};

/// Full cost query result.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub conversations: Vec<ConversationRollup>,
    pub roles: Vec<RoleRollup>,
    pub turn_growth: Vec<TurnGrowth>,
    pub unpriced: Vec<UnpricedLine>,
    /// `cached_prompt_tokens / prompt_tokens` over events that reported both; `None` if none did.
    pub cache_hit_rate: Option<f64>,
    /// Sum of priced call costs only — unpriced never contribute 0.0.
    pub total_cost_usd: Option<f64>,
    pub event_count: usize,
    pub priced_calls: usize,
    pub unpriced_calls: usize,
    pub total_repeat_calls: Option<usize>,
}

/// Format the report as plain-text tables for the CLI / PR paste.
pub fn format_report(report: &Report) -> String {
    let mut out = String::new();

    out.push_str("=== Liberado token cost report (read-time pricing) ===\n\n");
    out.push_str(&format!(
        "events: {}  priced_calls: {}  unpriced_calls: {}  repeat_calls: {}  total_usd: {}\n",
        report.event_count,
        report.priced_calls,
        report.unpriced_calls,
        match report.total_repeat_calls {
            Some(r) => r.to_string(),
            None => "n/a".into(),
        },
        fmt_money(report.total_cost_usd)
    ));
    out.push_str(&format!(
        "cache_hit_rate: {}\n\n",
        match report.cache_hit_rate {
            Some(r) => format!("{:.1}%", r * 100.0),
            None => "n/a".into(),
        }
    ));

    out.push_str("--- per conversation (includes delegated child correlations) ---\n");
    out.push_str(&format!(
        "{:<36} {:>6} {:>10} {:>10} {:>10} {:>12} {:>8}\n",
        "conversation", "calls", "prompt", "completion", "cached", "cost_usd", "unpriced"
    ));
    for c in &report.conversations {
        out.push_str(&format!(
            "{:<36} {:>6} {:>10} {:>10} {:>10} {:>12} {:>8}\n",
            truncate(&conversation_label(&c.conversation_id), 36),
            c.calls,
            fmt_opt_u32(c.prompt_tokens),
            fmt_opt_u32(c.completion_tokens),
            fmt_opt_u32(c.cached_prompt_tokens),
            fmt_money(c.cost_usd),
            c.unpriced_calls
        ));
        if !c.child_correlations.is_empty() {
            out.push_str(&format!(
                "  └ children: {}\n",
                c.child_correlations.join(", ")
            ));
        }
    }
    if report.conversations.is_empty() {
        out.push_str("(no conversations)\n");
    }

    out.push_str("\n--- per role ---\n");
    out.push_str(&format!(
        "{:<16} {:>6} {:>10} {:>10} {:>12} {:>8}\n",
        "role", "calls", "prompt", "completion", "cost_usd", "unpriced"
    ));
    for r in &report.roles {
        out.push_str(&format!(
            "{:<16} {:>6} {:>10} {:>10} {:>12} {:>8}\n",
            r.role,
            r.calls,
            fmt_opt_u32(r.prompt_tokens),
            fmt_opt_u32(r.completion_tokens),
            fmt_money(r.cost_usd),
            r.unpriced_calls
        ));
    }

    if !report.unpriced.is_empty() {
        out.push_str(
            "\n--- models with no usable rate (tokens known, cost unknown — never 0.0) ---\n",
        );
        out.push_str(&format!(
            "{:<40} {:>6} {:>10} {:>10} {:>10}\n",
            "model", "calls", "prompt", "completion", "cached"
        ));
        for u in &report.unpriced {
            out.push_str(&format!(
                "{:<40} {:>6} {:>10} {:>10} {:>10}\n",
                truncate(&u.model, 40),
                u.calls,
                fmt_opt_u32(u.prompt_tokens),
                fmt_opt_u32(u.completion_tokens),
                fmt_opt_u32(u.cached_prompt_tokens)
            ));
        }
    }

    if !report.turn_growth.is_empty() {
        out.push_str("\n--- prompt-token growth per turn (by conversation) ---\n");
        out.push_str(&format!(
            "{:<28} {:>5} {:>10} {:>10} {:>10} {:>12} {}\n",
            "conversation", "turn", "prompt", "delta", "role", "cost_usd", "correlation"
        ));
        // Cap to keep the default print usable; full data is in the struct.
        let limit = 50.min(report.turn_growth.len());
        for t in report.turn_growth.iter().take(limit) {
            out.push_str(&format!(
                "{:<28} {:>5} {:>10} {:>10} {:>10} {:>12} {}\n",
                truncate(&conversation_label(&t.conversation_id), 28),
                t.turn_index,
                fmt_opt_u32(t.prompt_tokens),
                match t.prompt_delta {
                    Some(d) => format!("{d:+}"),
                    None => "n/a".into(),
                },
                t.role,
                fmt_money(t.cost_usd),
                truncate(&t.correlation, 24)
            ));
        }
        if report.turn_growth.len() > limit {
            out.push_str(&format!(
                "... {} more turns omitted\n",
                report.turn_growth.len() - limit
            ));
        }
    }

    out
}

/// `latency::current_correlation()` yields `"-"` for any provider call made outside a
/// `with_correlation` scope, so those events all land in one bucket. Naming it keeps a row that
/// looks like a conversation id from being read as one — on the deployed journal it was 8% of
/// calls, which is a finding about instrumentation coverage, not a chat that cost money.
fn conversation_label(id: &str) -> String {
    if id == "-" {
        "(unattributed)".into()
    } else {
        id.to_string()
    }
}

fn fmt_money(v: Option<f64>) -> String {
    match v {
        Some(x) => format!("${x:.6}"),
        None => "null".into(),
    }
}

fn fmt_opt_u32(v: Option<u32>) -> String {
    match v {
        Some(x) => x.to_string(),
        None => "n/a".into(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}
