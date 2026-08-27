//! Environment-driven settings for the proxy binary, separated from `main` so the parsing rules
//! are unit-testable without touching process-global state.
//!
//! Rules, deliberately boring:
//! - a variable that is unset, empty, or whitespace counts as **absent** → default;
//! - numeric settings that fail to parse count as absent too — a typo'd TTL must not turn into
//!   `0` (refresh every request) silently;
//! - everything else passes through verbatim.

/// Where the proxy listens. Loopback by default and deliberately so: the proxy trusts its
/// callers and holds an upstream credential.
pub const DEFAULT_BIND: &str = "127.0.0.1:8788";
pub const DEFAULT_UPSTREAM_BASE: &str = "https://openrouter.ai/api/v1";
/// Six hours: benchmarks re-rank slowly and the API allows 500 requests/day per key.
pub const DEFAULT_TTL_SECS: u64 = 21_600;
/// Floor for the TTL knob. A configured 0 would mean "re-resolve on every request", hammering
/// the rate-limited benchmarks API (500/day) and adding discovery latency to every call — so
/// anything below one minute is treated as one minute, not as an error and not as zero.
pub const MIN_TTL_SECS: u64 = 60;
/// Default per-scrape budget for the fallback sources; the resolver's client also imports this
/// so the number lives in exactly one place.
pub const DEFAULT_SCRAPE_TIMEOUT_SECS: u64 = 90;
/// Default failover depth. Raised from 3 now that several free providers can share one ranking.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 6;

/// Everything `main` needs from the environment.
#[derive(Debug, Clone, PartialEq)]
pub struct ProxySettings {
    pub bind: String,
    pub upstream_base: String,
    pub ttl_secs: u64,
    pub max_attempts: u32,
    pub scrape_timeout_secs: u64,
}

impl ProxySettings {
    /// Read settings from the process environment.
    pub fn from_env() -> Self {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Read settings through `lookup` — the seam tests inject instead of mutating environ.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let get = |key: &str, default: &str| resolve_setting(lookup(key), default);
        let parse = |key: &str, default: u64| -> u64 {
            get(key, &default.to_string()).parse().unwrap_or(default)
        };
        Self {
            bind: get("LIBERADO_FREE_PROXY_BIND", DEFAULT_BIND),
            upstream_base: get("LIBERADO_FREE_PROXY_UPSTREAM_BASE", DEFAULT_UPSTREAM_BASE),
            ttl_secs: parse("LIBERADO_FREE_PROXY_TTL_SECS", DEFAULT_TTL_SECS).max(MIN_TTL_SECS),
            // Attempt depth is u32 upstream; parse through u64 so a nonsense negative reads as
            // absent rather than wrapping.
            max_attempts: parse(
                "LIBERADO_FREE_PROXY_MAX_ATTEMPTS",
                DEFAULT_MAX_ATTEMPTS as u64,
            )
            .min(u32::MAX as u64) as u32,
            scrape_timeout_secs: parse(
                "LIBERADO_FREE_PROXY_SCRAPE_TIMEOUT_SECS",
                DEFAULT_SCRAPE_TIMEOUT_SECS,
            ),
        }
    }
}

/// An env value counts as provided only when it has non-whitespace content.
pub fn resolve_setting(raw: Option<String>, default: &str) -> String {
    raw.filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup_of<'a>(map: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            map.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn unset_empty_and_whitespace_all_mean_default() {
        assert_eq!(resolve_setting(None, "d"), "d");
        assert_eq!(resolve_setting(Some(String::new()), "d"), "d");
        assert_eq!(resolve_setting(Some("   ".into()), "d"), "d");
    }

    #[test]
    fn a_set_value_passes_through_verbatim() {
        assert_eq!(resolve_setting(Some("x".into()), "d"), "x");
        assert_eq!(resolve_setting(Some(" spaced ".into()), "d"), " spaced ");
    }

    #[test]
    fn defaults_apply_when_nothing_is_set() {
        let s = ProxySettings::from_lookup(lookup_of(&[]));
        assert_eq!(s.bind, DEFAULT_BIND);
        assert_eq!(s.upstream_base, DEFAULT_UPSTREAM_BASE);
        assert_eq!(s.ttl_secs, DEFAULT_TTL_SECS);
        assert_eq!(s.max_attempts, DEFAULT_MAX_ATTEMPTS);
        assert_eq!(s.scrape_timeout_secs, DEFAULT_SCRAPE_TIMEOUT_SECS);
    }

    #[test]
    fn every_knob_is_read_from_its_named_variable() {
        let s = ProxySettings::from_lookup(lookup_of(&[
            ("LIBERADO_FREE_PROXY_BIND", "127.0.0.1:9999"),
            (
                "LIBERADO_FREE_PROXY_UPSTREAM_BASE",
                "https://mirror.example/api/v1",
            ),
            ("LIBERADO_FREE_PROXY_TTL_SECS", "60"),
            ("LIBERADO_FREE_PROXY_MAX_ATTEMPTS", "5"),
            ("LIBERADO_FREE_PROXY_SCRAPE_TIMEOUT_SECS", "17"),
        ]));
        assert_eq!(s.bind, "127.0.0.1:9999");
        assert_eq!(s.upstream_base, "https://mirror.example/api/v1");
        assert_eq!(s.ttl_secs, 60);
        assert_eq!(s.max_attempts, 5);
        assert_eq!(s.scrape_timeout_secs, 17);
    }

    #[test]
    fn garbage_numbers_fall_back_to_defaults_instead_of_zero() {
        // A typo'd TTL parsing to 0 would hammer the rate-limited benchmarks API every request;
        // absence semantics (use the default) are the safe reading of nonsense.
        let s = ProxySettings::from_lookup(lookup_of(&[
            ("LIBERADO_FREE_PROXY_TTL_SECS", "six-hours"),
            ("LIBERADO_FREE_PROXY_MAX_ATTEMPTS", ""),
            ("LIBERADO_FREE_PROXY_SCRAPE_TIMEOUT_SECS", "-4"),
        ]));
        assert_eq!(s.ttl_secs, DEFAULT_TTL_SECS);
        assert_eq!(s.max_attempts, DEFAULT_MAX_ATTEMPTS);
        assert_eq!(s.scrape_timeout_secs, DEFAULT_SCRAPE_TIMEOUT_SECS);
    }

    /// A parsed TTL of 0 is *not* nonsense — it parses cleanly, which makes it worse: it would
    /// silently mean "re-resolve every request". The floor catches the legitimate-looking case.
    #[test]
    fn ttl_values_below_the_floor_clamp_to_one_minute() {
        for configured in ["0", "1", "59"] {
            let s = ProxySettings::from_lookup(lookup_of(&[(
                "LIBERADO_FREE_PROXY_TTL_SECS",
                configured,
            )]));
            assert_eq!(s.ttl_secs, MIN_TTL_SECS, "configured {configured:?}");
        }
        let s = ProxySettings::from_lookup(lookup_of(&[("LIBERADO_FREE_PROXY_TTL_SECS", "61")]));
        assert_eq!(
            s.ttl_secs, 61,
            "at or above the floor passes through untouched"
        );
    }

    #[test]
    fn empty_bind_does_not_reach_the_listener() {
        // `TcpListener::bind("")` panics; blank must mean "use the default".
        let s = ProxySettings::from_lookup(lookup_of(&[("LIBERADO_FREE_PROXY_BIND", "")]));
        assert_eq!(s.bind, DEFAULT_BIND);
    }
}
