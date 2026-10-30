//! `[reminders]` in `tuning.toml` — durable reminder outbox defaults.

use serde::{Deserialize, Serialize};

/// Catch-up policy for mechanical reminder schedules. Model schedules stay `skip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatchUpPolicy {
    #[default]
    OnceWithin,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemindersConfig {
    pub catch_up: CatchUpPolicy,
    /// Human duration, e.g. `6h`.
    pub once_within: String,
    pub max_catchup_per_boot: u32,
    pub max_attempts: u32,
    pub channels: ReminderChannelsConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ReminderChannelsConfig {
    pub telegram: TelegramReminderConfig,
    pub matrix: MatrixReminderConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TelegramReminderConfig {
    /// `fallback` | `always` | `off`
    #[serde(default = "default_telegram_mode")]
    pub mode: String,
}

fn default_telegram_mode() -> String {
    "fallback".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MatrixReminderConfig {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
}

impl Default for RemindersConfig {
    fn default() -> Self {
        Self {
            catch_up: CatchUpPolicy::OnceWithin,
            once_within: "6h".into(),
            max_catchup_per_boot: 20,
            max_attempts: 8,
            channels: ReminderChannelsConfig::default(),
        }
    }
}

impl Default for TelegramReminderConfig {
    fn default() -> Self {
        Self {
            mode: default_telegram_mode(),
        }
    }
}

impl RemindersConfig {
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !matches!(
            self.channels.telegram.mode.as_str(),
            "fallback" | "always" | "off"
        ) {
            out.push(format!(
                "reminders.channels.telegram.mode must be fallback, always, or off (got '{}')",
                self.channels.telegram.mode
            ));
        }
        if self.max_catchup_per_boot == 0 {
            out.push("reminders.max_catchup_per_boot must be at least 1".into());
        }
        if self.max_attempts == 0 {
            out.push("reminders.max_attempts must be at least 1".into());
        }
        if parse_duration(&self.once_within).is_none() {
            out.push(format!(
                "reminders.once_within is not a valid duration ('{}')",
                self.once_within
            ));
        }
        out
    }

    pub fn once_within_secs(&self) -> Option<u64> {
        parse_duration(&self.once_within)
    }
}

fn parse_duration(text: &str) -> Option<u64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(hours) = trimmed.strip_suffix('h') {
        let n: u64 = hours.parse().ok()?;
        return Some(n.saturating_mul(3600));
    }
    if let Some(minutes) = trimmed.strip_suffix('m') {
        let n: u64 = minutes.parse().ok()?;
        return Some(n.saturating_mul(60));
    }
    if let Some(seconds) = trimmed.strip_suffix('s') {
        return seconds.parse().ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_table_matches_plan_defaults() {
        let cfg = RemindersConfig::default();
        assert_eq!(cfg.catch_up, CatchUpPolicy::OnceWithin);
        assert_eq!(cfg.once_within, "6h");
        assert_eq!(cfg.max_catchup_per_boot, 20);
        assert_eq!(cfg.max_attempts, 8);
        assert_eq!(cfg.channels.telegram.mode, "fallback");
        assert!(!cfg.channels.matrix.enabled);
        assert!(cfg.problems().is_empty());
    }

    #[test]
    fn invalid_telegram_mode_is_reported() {
        let mut cfg = RemindersConfig::default();
        cfg.channels.telegram.mode = "dual".into();
        assert!(
            cfg.problems()
                .iter()
                .any(|line| line.contains("telegram.mode"))
        );
    }
}
