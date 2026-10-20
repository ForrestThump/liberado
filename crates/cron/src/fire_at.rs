//! Next UTC instant for a parsed cron expression.
//!
//! `zone == None` reads the fields as UTC. `Some` reads them as civil time in
//! that zone, which is how a Tuesday 20:00 stays 20:00 across DST.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;

pub(crate) fn next_utc(
    parsed: &cron::Schedule,
    zone: Option<Tz>,
    after: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    match zone {
        None => parsed.after(&after).next(),
        Some(zone) => {
            let local = after.with_timezone(&zone);
            parsed
                .after(&local)
                .next()
                .map(|civil| civil.with_timezone(&Utc))
        }
    }
}

/// `None` means the cron fields are UTC. A blank name is also UTC.
pub(crate) fn eval_zone(raw: Option<&str>) -> Result<Option<Tz>, String> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    text.parse::<Tz>().map(Some).map_err(|_| text.to_string())
}
