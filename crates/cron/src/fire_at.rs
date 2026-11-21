//! Next UTC instant for a parsed cron expression.
//!
//! `zone == None` reads the fields as UTC. `Some` reads them as civil time in
//! that zone, which is how a Tuesday 20:00 stays 20:00 across DST.

use jiff::tz::TimeZone;
use liberado_common::UtcStamp;

pub(crate) fn next_utc(
    parsed: &jiff_cron::Schedule,
    zone: Option<&TimeZone>,
    after: UtcStamp,
) -> Option<UtcStamp> {
    let zoned = match zone {
        None => after.timestamp().to_zoned(TimeZone::UTC),
        Some(zone) => after.timestamp().to_zoned(zone.clone()),
    };
    parsed
        .after(zoned)
        .next()
        .map(|next| UtcStamp::from_timestamp(next.timestamp()))
}

/// `None` means the cron fields are UTC. A blank name is also UTC.
pub(crate) fn eval_zone(raw: Option<&str>) -> Result<Option<TimeZone>, String> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    TimeZone::get(text).map(Some).map_err(|_| text.to_string())
}
