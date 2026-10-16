//! Field and atom parsing for a cron expression.
//!
//! Split from `english.rs` so that file stays inside the module-health boundaries.

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Atom {
    Any,
    Value(u32),
    Step { start: u32, step: u32 },
    Range { start: u32, end: u32 },
    List(Vec<u32>),
}

#[derive(Clone, Copy)]
enum FieldKind {
    Second,
    Minute,
    Hour,
    Dom,
    Dow { five: bool },
}

pub(super) struct Parsed {
    pub(super) second: Atom,
    pub(super) minute: Atom,
    pub(super) hour: Atom,
    pub(super) dom: Atom,
    pub(super) month_any: bool,
    pub(super) dow: Atom,
    pub(super) year_any: bool,
}

pub(super) fn parse_expression(raw: &str) -> Option<(Parsed, bool)> {
    if let Some(expanded) = expand_shorthand(raw) {
        let tokens: Vec<&str> = expanded.split_whitespace().collect();
        return Some((parse_tokens(&tokens, false)?, false));
    }
    let tokens: Vec<&str> = raw.split_whitespace().collect();
    match tokens.len() {
        5 => {
            let mut fields = Vec::with_capacity(6);
            fields.push("0");
            fields.extend(tokens);
            Some((parse_tokens(&fields, true)?, true))
        }
        6 => Some((parse_tokens(&tokens, false)?, false)),
        7 => Some((parse_tokens(&tokens, false)?, false)),
        _ => None,
    }
}

fn expand_shorthand(raw: &str) -> Option<&'static str> {
    match raw {
        "@yearly" => Some("0 0 0 1 1 * *"),
        "@monthly" => Some("0 0 0 1 * * *"),
        "@weekly" => Some("0 0 0 * * 1 *"),
        "@daily" => Some("0 0 0 * * * *"),
        "@hourly" => Some("0 0 * * * * *"),
        _ => None,
    }
}

fn parse_tokens(tokens: &[&str], five: bool) -> Option<Parsed> {
    let year_any = match tokens.len() {
        6 => true,
        7 => tokens[6] == "*" || tokens[6] == "?",
        _ => return None,
    };
    Some(Parsed {
        second: parse_atom(tokens[0], FieldKind::Second)?,
        minute: parse_atom(tokens[1], FieldKind::Minute)?,
        hour: parse_atom(tokens[2], FieldKind::Hour)?,
        dom: parse_atom(tokens[3], FieldKind::Dom)?,
        month_any: tokens[4] == "*" || tokens[4] == "?",
        dow: parse_atom(tokens[5], FieldKind::Dow { five })?,
        year_any,
    })
}

fn parse_atom(raw: &str, kind: FieldKind) -> Option<Atom> {
    if raw == "*" || raw == "?" {
        return Some(Atom::Any);
    }
    if let Some((start, step)) = raw.split_once('/') {
        if start.contains('/') {
            return None;
        }
        let start = if start == "*" {
            0
        } else {
            resolve_token(start, kind)?
        };
        let step = start_step(step)?;
        return Some(Atom::Step { start, step });
    }
    if raw.contains(',') {
        return parse_list(raw, kind);
    }
    if let Some((left, right)) = raw.split_once('-') {
        let start = resolve_token(left, kind)?;
        let end = resolve_token(right, kind)?;
        if start > end {
            return None;
        }
        return Some(Atom::Range { start, end });
    }
    Some(Atom::Value(resolve_token(raw, kind)?))
}

fn start_step(raw: &str) -> Option<u32> {
    let step = raw.parse::<u32>().ok()?;
    (step > 0).then_some(step)
}

fn parse_list(raw: &str, kind: FieldKind) -> Option<Atom> {
    let mut values = Vec::new();
    for part in raw.split(',') {
        if part.is_empty() || part.contains('-') || part.contains('/') || part.contains('*') {
            return None;
        }
        values.push(resolve_token(part, kind)?);
    }
    if values.is_empty() || values.len() > 31 {
        return None;
    }
    values.sort_unstable();
    values.dedup();
    Some(Atom::List(values))
}

fn resolve_token(token: &str, kind: FieldKind) -> Option<u32> {
    if let Ok(number) = token.parse::<u32>() {
        return match kind {
            FieldKind::Second | FieldKind::Minute => in_range(number, 0, 59),
            FieldKind::Hour => in_range(number, 0, 23),
            FieldKind::Dom => in_range(number, 1, 31),
            FieldKind::Dow { five: false } => in_range(number, 1, 7),
            FieldKind::Dow { five: true } => five_field_dow(number),
        };
    }
    match kind {
        FieldKind::Dow { .. } => dow_name(token),
        _ => None,
    }
}

fn in_range(number: u32, min: u32, max: u32) -> Option<u32> {
    (min..=max).contains(&number).then_some(number)
}

fn five_field_dow(number: u32) -> Option<u32> {
    match number {
        0 | 7 => Some(1),
        1..=6 => Some(number + 1),
        _ => None,
    }
}

fn dow_name(token: &str) -> Option<u32> {
    match token.to_ascii_lowercase().as_str() {
        "sun" | "sunday" => Some(1),
        "mon" | "monday" => Some(2),
        "tue" | "tues" | "tuesday" => Some(3),
        "wed" | "wednesday" => Some(4),
        "thu" | "thurs" | "thursday" => Some(5),
        "fri" | "friday" => Some(6),
        "sat" | "saturday" => Some(7),
        _ => None,
    }
}

impl Atom {
    pub(super) fn is_any(&self) -> bool {
        matches!(self, Atom::Any)
    }
}
