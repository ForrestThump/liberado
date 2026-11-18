use crate::error::{Error, Result};
use liberado_common::UtcStamp;

pub fn stamp(at: UtcStamp) -> String {
    at.to_rfc3339_opts(liberado_common::Rfc3339Seconds::Secs, true)
}

pub fn parse_stamp(text: &str) -> Result<UtcStamp> {
    text.parse::<UtcStamp>()
        .map_err(|err| Error::Time(err.to_string()))
}
