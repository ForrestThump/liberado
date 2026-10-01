use chrono::NaiveDate;

pub(super) fn parse_ymd(value: &str) -> Option<NaiveDate> {
    let value = value.trim();
    let date = value.get(..10)?;
    if value.len() > 10 {
        let boundary = value.as_bytes().get(10).copied()?;
        if boundary != b'T' && boundary != b' ' {
            return None;
        }
    }
    NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()
}
