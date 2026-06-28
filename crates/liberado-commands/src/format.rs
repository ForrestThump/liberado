use crate::constants::{SECS_IN_HOUR, SECS_IN_MINUTE};

pub fn format_uptime(seconds: u64) -> String {
    let h = seconds / SECS_IN_HOUR;
    let m = (seconds % SECS_IN_HOUR) / SECS_IN_MINUTE;
    if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m {}s", seconds % 60)
    }
}
