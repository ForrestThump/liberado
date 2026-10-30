use chrono::{DateTime, SecondsFormat, Utc};

use crate::error::{Error, Result};

pub fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub fn parse_stamp(text: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|err| Error::Time(err.to_string()))
}
