//! Absolute instants with the RFC 3339 text chrono wrote.
//!
//! `chrono::DateTime<Utc>::to_rfc3339` uses a numeric offset (`+00:00`), and its
//! `serde` feature uses `Z`. Fractional seconds stay in groups of three digits
//! (`.100`, `.123400`, `.000000001`). Proposal HMAC input, outbox rows, JSONL
//! logs, and webhook JSON were written that way. [`UtcStamp`] reads both shapes
//! and writes the same bytes back.
//!
//! [`jiff::Timestamp`]'s own `Display` trims trailing zeros without the
//! three-digit groups, and a [`jiff::Zoned`] `Display` appends `[Area/City]`.
//! Neither string is what Liberado stored. This module is the boundary.

use std::fmt;
use std::ops::{Add, AddAssign, Sub, SubAssign};

use jiff::tz::Offset;
use jiff::{SignedDuration, Timestamp, Zoned};
use serde::{Deserialize, Serialize};

/// How many fractional digits to emit. Matches `chrono::SecondsFormat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rfc3339Seconds {
    /// No fractional part, even when the instant has subseconds.
    Secs,
    /// Always three digits. Extra precision is truncated.
    Millis,
    /// Always six digits. Extra precision is truncated.
    Micros,
    /// Always nine digits.
    Nanos,
    /// The minimum multiple of three digits that keeps every non-zero digit.
    Auto,
}

/// An absolute UTC instant.
///
/// Serde is `chrono`'s `DateTime<Utc>` serde: RFC 3339 with `Z` and
/// [`Rfc3339Seconds::Auto`]. [`UtcStamp::to_rfc3339`] is `chrono`'s method of
/// the same name: the same digits with `+00:00` instead of `Z`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcStamp(Timestamp);

impl UtcStamp {
    pub fn now() -> Self {
        Self(Timestamp::now())
    }

    pub fn from_timestamp(timestamp: Timestamp) -> Self {
        Self(timestamp)
    }

    pub fn timestamp(self) -> Timestamp {
        self.0
    }

    /// Whole seconds since the Unix epoch. Matches `DateTime::timestamp`.
    pub fn unix_seconds(self) -> i64 {
        self.0.as_second()
    }

    /// Whole milliseconds since the Unix epoch. Matches `timestamp_millis`.
    pub fn unix_millis(self) -> i64 {
        self.0.as_millisecond()
    }

    /// `YYYY-MM-DDTHH:MM:SS+00:00`, with chrono's auto fractional digits.
    pub fn to_rfc3339(self) -> String {
        format_rfc3339(self.0, Offset::UTC, Rfc3339Seconds::Auto, false)
    }

    /// `to_rfc3339_opts`. `use_z` writes `Z` when the offset is zero.
    pub fn to_rfc3339_opts(self, seconds: Rfc3339Seconds, use_z: bool) -> String {
        format_rfc3339(self.0, Offset::UTC, seconds, use_z)
    }

    pub fn duration_since(self, earlier: Self) -> SignedDuration {
        self.0.duration_since(earlier.0)
    }

    pub fn checked_add(self, duration: SignedDuration) -> Option<Self> {
        self.0.checked_add(duration).ok().map(Self)
    }

    pub fn checked_sub(self, duration: SignedDuration) -> Option<Self> {
        self.0.checked_sub(duration).ok().map(Self)
    }

    /// UTC civil fields. `None` when the combination is not a real civil time.
    pub fn from_civil_utc(
        year: i16,
        month: i8,
        day: i8,
        hour: i8,
        minute: i8,
        second: i8,
    ) -> Option<Self> {
        let civil = jiff::civil::DateTime::new(year, month, day, hour, minute, second, 0).ok()?;
        let zoned = civil.to_zoned(jiff::tz::TimeZone::UTC).ok()?;
        Some(Self(zoned.timestamp()))
    }

    pub fn from_unix_seconds(seconds: i64) -> Option<Self> {
        Timestamp::from_second(seconds).ok().map(Self)
    }

    /// Latest representable instant. Sorts after every parsed stamp.
    pub fn max_value() -> Self {
        Self(Timestamp::MAX)
    }

    /// `strftime` on the UTC instant. Specifiers match jiff (`%Y`, `%H`, `%.3f`).
    pub fn strftime(&self, format: &str) -> String {
        self.0.strftime(format).to_string()
    }
}

impl Default for UtcStamp {
    fn default() -> Self {
        Self(Timestamp::UNIX_EPOCH)
    }
}

impl fmt::Debug for UtcStamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("UtcStamp")
            .field(&self.to_rfc3339_opts(Rfc3339Seconds::Auto, true))
            .finish()
    }
}

impl fmt::Display for UtcStamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rfc3339())
    }
}

impl std::str::FromStr for UtcStamp {
    type Err = jiff::Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse::<Timestamp>().map(Self)
    }
}

impl From<Timestamp> for UtcStamp {
    fn from(timestamp: Timestamp) -> Self {
        Self(timestamp)
    }
}

impl TryFrom<std::time::SystemTime> for UtcStamp {
    type Error = jiff::Error;

    fn try_from(time: std::time::SystemTime) -> Result<Self, Self::Error> {
        Timestamp::try_from(time).map(Self)
    }
}

impl From<UtcStamp> for Timestamp {
    fn from(stamp: UtcStamp) -> Self {
        stamp.0
    }
}

impl Add<SignedDuration> for UtcStamp {
    type Output = Self;

    fn add(self, duration: SignedDuration) -> Self {
        Self(self.0 + duration)
    }
}

impl Sub<SignedDuration> for UtcStamp {
    type Output = Self;

    fn sub(self, duration: SignedDuration) -> Self {
        Self(self.0 - duration)
    }
}

impl Sub<UtcStamp> for UtcStamp {
    type Output = SignedDuration;

    fn sub(self, earlier: UtcStamp) -> SignedDuration {
        self.0.duration_since(earlier.0)
    }
}

impl AddAssign<SignedDuration> for UtcStamp {
    fn add_assign(&mut self, duration: SignedDuration) {
        *self = *self + duration;
    }
}

impl SubAssign<SignedDuration> for UtcStamp {
    fn sub_assign(&mut self, duration: SignedDuration) {
        *self = *self - duration;
    }
}

impl Serialize for UtcStamp {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_rfc3339_opts(Rfc3339Seconds::Auto, true))
    }
}

impl<'de> Deserialize<'de> for UtcStamp {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse::<UtcStamp>().map_err(serde::de::Error::custom)
    }
}

/// RFC 3339 for `timestamp` shown at `offset`.
///
/// `use_z` writes `Z` only when `offset` is zero. A non-zero offset always
/// uses `±HH:MM` or `±HH:MM:SS`.
pub fn format_rfc3339(
    timestamp: Timestamp,
    offset: Offset,
    seconds: Rfc3339Seconds,
    use_z: bool,
) -> String {
    let civil = offset.to_datetime(timestamp);
    let fraction = fractional(civil.subsec_nanosecond(), seconds);
    let suffix = offset_suffix(offset, use_z);
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{fraction}{suffix}",
        year = civil.year(),
        month = civil.month(),
        day = civil.day(),
        hour = civil.hour(),
        minute = civil.minute(),
        second = civil.second(),
    )
}

/// `DateTime<Tz>::to_rfc3339`: local civil time, numeric offset, auto digits.
pub fn format_zoned_rfc3339(zoned: &Zoned) -> String {
    format_rfc3339(
        zoned.timestamp(),
        zoned.offset(),
        Rfc3339Seconds::Auto,
        false,
    )
}

fn fractional(nanos: i32, seconds: Rfc3339Seconds) -> String {
    let nanos = nanos.unsigned_abs();
    match seconds {
        Rfc3339Seconds::Secs => String::new(),
        Rfc3339Seconds::Millis => format!(".{:03}", nanos / 1_000_000),
        Rfc3339Seconds::Micros => format!(".{:06}", nanos / 1_000),
        Rfc3339Seconds::Nanos => format!(".{nanos:09}"),
        Rfc3339Seconds::Auto => {
            if nanos == 0 {
                return String::new();
            }
            let mut digits = format!("{nanos:09}");
            while digits.len() > 3 && digits.ends_with("000") {
                digits.truncate(digits.len() - 3);
            }
            format!(".{digits}")
        }
    }
}

fn offset_suffix(offset: Offset, use_z: bool) -> String {
    let total = offset.seconds();
    if use_z && total == 0 {
        return "Z".to_string();
    }
    let sign = if total < 0 { '-' } else { '+' };
    let total = total.unsigned_abs();
    let hours = total / 3_600;
    let minutes = (total % 3_600) / 60;
    let seconds = total % 60;
    if seconds == 0 {
        format!("{sign}{hours:02}:{minutes:02}")
    } else {
        format!("{sign}{hours:02}:{minutes:02}:{seconds:02}")
    }
}

/// Absolute day length chrono used: 86_400 seconds, not a calendar day.
pub fn duration_days(days: i64) -> SignedDuration {
    SignedDuration::from_secs(days.saturating_mul(86_400))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::tz::TimeZone;

    fn at(seconds: i64, nanos: i32) -> UtcStamp {
        UtcStamp(Timestamp::new(seconds, nanos).unwrap())
    }

    #[test]
    fn rfc3339_matches_chrono_autos_i_samples() {
        let cases = [
            (0, "2025-06-27T04:53:20+00:00", "2025-06-27T04:53:20Z"),
            (
                123_000_000,
                "2025-06-27T04:53:20.123+00:00",
                "2025-06-27T04:53:20.123Z",
            ),
            (
                123_400_000,
                "2025-06-27T04:53:20.123400+00:00",
                "2025-06-27T04:53:20.123400Z",
            ),
            (
                100_000_000,
                "2025-06-27T04:53:20.100+00:00",
                "2025-06-27T04:53:20.100Z",
            ),
            (
                1,
                "2025-06-27T04:53:20.000000001+00:00",
                "2025-06-27T04:53:20.000000001Z",
            ),
            (
                123_456_789,
                "2025-06-27T04:53:20.123456789+00:00",
                "2025-06-27T04:53:20.123456789Z",
            ),
        ];
        for (nanos, offset_form, zulu) in cases {
            let stamp = at(1_751_000_000, nanos);
            assert_eq!(stamp.to_rfc3339(), offset_form, "{nanos}");
            assert_eq!(
                stamp.to_rfc3339_opts(Rfc3339Seconds::Auto, true),
                zulu,
                "{nanos}"
            );
            let json = serde_json::to_string(&stamp).unwrap();
            assert_eq!(json, format!("\"{zulu}\""), "{nanos}");
            let back: UtcStamp = serde_json::from_str(&json).unwrap();
            assert_eq!(back, stamp);
        }
    }

    #[test]
    fn fixed_precision_truncates_like_chrono_seconds_format() {
        let stamp = at(1_751_000_000, 123_456_789);
        assert_eq!(
            stamp.to_rfc3339_opts(Rfc3339Seconds::Secs, true),
            "2025-06-27T04:53:20Z"
        );
        assert_eq!(
            stamp.to_rfc3339_opts(Rfc3339Seconds::Millis, true),
            "2025-06-27T04:53:20.123Z"
        );
        assert_eq!(
            stamp.to_rfc3339_opts(Rfc3339Seconds::Micros, true),
            "2025-06-27T04:53:20.123456Z"
        );
        let whole = at(1_751_000_000, 0);
        assert_eq!(
            whole.to_rfc3339_opts(Rfc3339Seconds::Millis, true),
            "2025-06-27T04:53:20.000Z"
        );
    }

    #[test]
    fn reads_chrono_offset_and_zulu_text() {
        let zulu: UtcStamp = "2026-01-02T03:04:05Z".parse().unwrap();
        let offset: UtcStamp = "2026-01-02T03:04:05+00:00".parse().unwrap();
        let padded: UtcStamp = "2025-06-27T04:53:20.100Z".parse().unwrap();
        assert_eq!(zulu, offset);
        assert_eq!(zulu.to_rfc3339(), "2026-01-02T03:04:05+00:00");
        assert_eq!(padded.to_rfc3339(), "2025-06-27T04:53:20.100+00:00");
        let chicago: UtcStamp = "2026-07-19T21:32:00-05:00".parse().unwrap();
        assert_eq!(chicago.to_rfc3339(), "2026-07-20T02:32:00+00:00");
    }

    #[test]
    fn zoned_rfc3339_is_the_civil_offset_without_a_zone_name() {
        let zone = TimeZone::get("America/Chicago").unwrap();
        let zoned = jiff::civil::DateTime::new(2026, 7, 19, 21, 32, 0, 0)
            .unwrap()
            .to_zoned(zone)
            .unwrap();
        assert_eq!(format_zoned_rfc3339(&zoned), "2026-07-19T21:32:00-05:00");
        assert_eq!(zoned.strftime("%Z").to_string(), "CDT");
    }
}
