use jiff::civil::{Date, Time};

pub(super) fn parse_ymd(value: &str) -> Option<Date> {
    let value = value.trim();
    let date = value.get(..10)?;
    if value.len() > 10 {
        let boundary = value.as_bytes().get(10).copied()?;
        if boundary != b'T' && boundary != b' ' {
            return None;
        }
    }
    date.parse().ok()
}

/// Task dates still come from turbovault. Their text is `YYYY-MM-DD`.
pub(super) fn date_from_display(date: &impl std::fmt::Display) -> Option<Date> {
    date.to_string().parse().ok()
}

pub(super) fn format_hhmm(time: Time) -> String {
    format!("{:02}:{:02}", time.hour(), time.minute())
}

pub(super) fn time_hms(hour: u32, minute: u32, second: u32) -> Option<Time> {
    let hour = i8::try_from(hour).ok()?;
    let minute = i8::try_from(minute).ok()?;
    let second = i8::try_from(second).ok()?;
    Time::new(hour, minute, second, 0).ok()
}
