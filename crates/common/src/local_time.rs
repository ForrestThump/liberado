//! Single source of truth for the operator's **local timezone**, and helpers to stamp
//! "what time is it here?" onto agent context when that matters.
//!
//! Config owns the IANA name (`topology.timezone`, e.g. `America/Chicago`). This module is the
//! pure clock/format layer every crate can call without pulling config I/O:
//!
//! ```ignore
//! let tz = UserTimezone::parse("America/Chicago")?;
//! let goal = tz.with_context("Summarize today's calendar.");
//! // → "Local time: 2026-07-19 21:32 CDT (America/Chicago).\n\nSummarize today's calendar."
//! ```
//!
//! **Not** injected into every system prompt by default — callers opt in (cron/webhook firings
//! do this automatically in the daemon). Use [`UserTimezone::context_line`] / [`with_context`]
//! anywhere else (face chat, wake-up body, a tool description) when local wall-clock helps.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;

/// Default IANA zone for this Life OS installation (Texas / US Central).
///
/// Override per-deploy with `topology.timezone` in `topology.toml`. Keep this in sync with the
/// default on [`liberado_config_loader::Topology`] — both are the same SSoT string.
pub const DEFAULT_TIMEZONE: &str = "America/Chicago";

/// A validated IANA timezone used to format local wall-clock for agent context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserTimezone {
    tz: Tz,
}

/// Failed to resolve an IANA timezone name.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("unknown IANA timezone '{0}' (use e.g. America/Chicago, America/Denver, UTC)")]
pub struct UnknownTimezone(pub String);

impl UserTimezone {
    /// Parse an IANA name (`America/Chicago`, `UTC`, …). Empty/whitespace is rejected.
    pub fn parse(iana: &str) -> Result<Self, UnknownTimezone> {
        let name = iana.trim();
        if name.is_empty() {
            return Err(UnknownTimezone(iana.to_string()));
        }
        let tz: Tz = name
            .parse()
            .map_err(|_| UnknownTimezone(name.to_string()))?;
        Ok(Self { tz })
    }

    /// The installation default ([`DEFAULT_TIMEZONE`]). Infallible.
    pub fn default_zone() -> Self {
        Self::parse(DEFAULT_TIMEZONE).expect("DEFAULT_TIMEZONE must be a valid IANA name")
    }

    /// IANA name as stored in config (e.g. `"America/Chicago"`).
    pub fn iana_name(&self) -> &str {
        self.tz.name()
    }

    /// Current wall-clock in this zone.
    pub fn now(&self) -> DateTime<Tz> {
        Utc::now().with_timezone(&self.tz)
    }

    /// Convert a UTC instant into this zone.
    pub fn at(&self, utc: DateTime<Utc>) -> DateTime<Tz> {
        utc.with_timezone(&self.tz)
    }

    /// One short line for agent context, stamped **now**.
    ///
    /// Example: `Local time: 2026-07-19 21:32 CDT (America/Chicago).`
    pub fn context_line(&self) -> String {
        self.context_line_at(Utc::now())
    }

    /// Same as [`context_line`](Self::context_line) for a specific UTC instant (tests / event time).
    pub fn context_line_at(&self, utc: DateTime<Utc>) -> String {
        let local = self.at(utc);
        // `%Z` is the abbreviated zone (CDT/CST); IANA name in parens is unambiguous year-round.
        format!(
            "Local time: {} {} ({}).",
            local.format("%Y-%m-%d %H:%M"),
            local.format("%Z"),
            self.iana_name()
        )
    }

    /// Prepend [`context_line`](Self::context_line) to a goal / prompt body.
    ///
    /// Empty body → just the line. Non-empty → line, blank line, body.
    pub fn with_context(&self, body: &str) -> String {
        self.with_context_at(Utc::now(), body)
    }

    /// [`with_context`](Self::with_context) at a fixed UTC instant.
    pub fn with_context_at(&self, utc: DateTime<Utc>, body: &str) -> String {
        let line = self.context_line_at(utc);
        let body = body.trim();
        if body.is_empty() {
            line
        } else {
            format!("{line}\n\n{body}")
        }
    }
}

impl Default for UserTimezone {
    fn default() -> Self {
        Self::default_zone()
    }
}

impl std::str::FromStr for UserTimezone {
    type Err = UnknownTimezone;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Convenience: format a context line from a raw IANA string without keeping a [`UserTimezone`].
pub fn context_line(iana: &str) -> Result<String, UnknownTimezone> {
    Ok(UserTimezone::parse(iana)?.context_line())
}

/// Convenience: prepend local-now to `body` using the given IANA zone name.
pub fn with_context(iana: &str, body: &str) -> Result<String, UnknownTimezone> {
    Ok(UserTimezone::parse(iana)?.with_context(body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn default_zone_parses() {
        let tz = UserTimezone::default_zone();
        assert_eq!(tz.iana_name(), "America/Chicago");
    }

    #[test]
    fn rejects_unknown_and_empty() {
        assert!(UserTimezone::parse("Not/AZone").is_err());
        assert!(UserTimezone::parse("").is_err());
        assert!(UserTimezone::parse("   ").is_err());
    }

    #[test]
    fn context_line_includes_iana_and_local_wall_clock() {
        // Fixed UTC: 2026-07-20 02:32 UTC = 2026-07-19 21:32 CDT (America/Chicago, UTC-5 in July).
        let utc = Utc.with_ymd_and_hms(2026, 7, 20, 2, 32, 0).unwrap();
        let tz = UserTimezone::parse("America/Chicago").unwrap();
        let line = tz.context_line_at(utc);
        assert!(
            line.contains("2026-07-19 21:32"),
            "expected CDT wall clock in line, got {line}"
        );
        assert!(line.contains("America/Chicago"), "got {line}");
        assert!(line.starts_with("Local time:"), "got {line}");
    }

    #[test]
    fn with_context_prefixes_body() {
        let utc = Utc.with_ymd_and_hms(2026, 7, 20, 2, 32, 0).unwrap();
        let tz = UserTimezone::parse("America/Chicago").unwrap();
        let out = tz.with_context_at(utc, "Summarize today's calendar.");
        assert!(out.starts_with("Local time:"));
        assert!(out.contains("\n\nSummarize today's calendar."));
    }

    #[test]
    fn with_context_empty_body_is_just_the_line() {
        let tz = UserTimezone::default_zone();
        let out = tz.with_context("  ");
        assert!(out.starts_with("Local time:"));
        assert!(!out.contains("\n\n"));
    }

    #[test]
    fn free_functions_round_trip() {
        let line = context_line("UTC").unwrap();
        assert!(line.contains("(UTC)"));
        let body = with_context("UTC", "do the thing").unwrap();
        assert!(body.contains("do the thing"));
    }

    /// The FromStr impl delegates to UserTimezone::parse. This test goes through the trait method
    /// to catch mutations that replace `from_str` with `Ok(Default::default())`.
    #[test]
    fn from_str_rejects_unknown_zone() {
        use std::str::FromStr;
        assert!(UserTimezone::from_str("Not/AZone").is_err());
        assert!(UserTimezone::from_str("").is_err());
        // A known zone round-trips correctly.
        assert!(UserTimezone::from_str("UTC").is_ok());
    }
}
