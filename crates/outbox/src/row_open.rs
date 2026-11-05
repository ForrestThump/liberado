use chrono::Utc;
use rusqlite::Row;

use crate::kind;
use crate::plan::OpenRow;
use crate::source::OutboxSource;
use crate::time::parse_stamp;

pub fn read_open(row: &Row<'_>) -> rusqlite::Result<OpenRow> {
    let due: String = row.get(6)?;
    let token: String = row.get(7)?;
    let kind_text: Option<String> = row.get(8)?;
    let source_text: Option<String> = row.get(9)?;
    Ok(OpenRow {
        id: row.get(0)?,
        channel: row.get(1)?,
        target: row.get(2)?,
        title: row.get(3)?,
        body: row.get(4)?,
        catch_up: row.get::<_, i64>(5)? != 0,
        due_at: parse_stamp(&due).unwrap_or_else(|_| Utc::now()),
        token,
        kind: kind::parse_stored(kind_text.as_deref()).to_string(),
        source: OutboxSource::decode(source_text),
    })
}

pub fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}
