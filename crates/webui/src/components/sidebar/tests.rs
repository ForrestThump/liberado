//! Sidebar helper tests.
//!
//! Split from `sidebar.rs` for module-health boundaries.

use super::agents::{pinned, row_dot_color, truncate_title};
use super::{delete_accepted, relative_time};

fn ago(dur: chrono::Duration) -> String {
    (chrono::Utc::now() - dur).to_rfc3339()
}

#[test]
fn unparseable_is_empty() {
    assert_eq!(relative_time("not-a-date"), "");
}

#[test]
fn future_reads_as_just_now() {
    assert_eq!(
        relative_time(&ago(-chrono::Duration::minutes(1))),
        "just now"
    );
}

#[test]
fn age_bands_choose_the_unit() {
    assert_eq!(
        relative_time(&ago(chrono::Duration::seconds(12))),
        "12s ago"
    );
    assert_eq!(relative_time(&ago(chrono::Duration::minutes(5))), "5m ago");
    assert_eq!(relative_time(&ago(chrono::Duration::hours(3))), "3h ago");
    assert_eq!(relative_time(&ago(chrono::Duration::days(2))), "2d ago");
}

#[test]
fn past_a_month_shows_the_date() {
    assert_eq!(relative_time("2000-01-15T10:00:00Z"), "Jan 15");
}

#[test]
fn offsetless_timestamp_is_treated_as_utc() {
    let raw = (chrono::Utc::now() - chrono::Duration::minutes(7))
        .format("%Y-%m-%dT%H:%M:%S%.f")
        .to_string();
    assert_eq!(relative_time(&raw), "7m ago");
}

#[test]
fn band_boundaries_fall_up() {
    let at = |secs: i64| (chrono::Utc::now() - chrono::Duration::seconds(secs)).to_rfc3339();
    assert_eq!(relative_time(&at(60)), "1m ago");
    assert_eq!(relative_time(&at(3600)), "1h ago");
    assert_eq!(relative_time(&at(86_400)), "1d ago");
}

#[test]
fn exactly_thirty_days_shows_the_date() {
    let raw = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
    let expected = chrono::DateTime::parse_from_rfc3339(&raw)
        .unwrap()
        .format("%b %d")
        .to_string();
    assert_eq!(relative_time(&raw), expected);
}

#[test]
fn sub_second_ages_from_zero() {
    let raw = (chrono::Utc::now() - chrono::Duration::milliseconds(500)).to_rfc3339();
    assert_eq!(relative_time(&raw), "0s ago");
}

#[test]
fn delete_accepts_2xx_and_404() {
    for ok in [200, 204, 299, 404] {
        assert!(delete_accepted(ok), "{ok} should delete cleanly");
    }
    for no in [300, 403, 500] {
        assert!(
            !delete_accepted(no),
            "{no} should surface as a delete failure"
        );
    }
}

#[test]
fn long_titles_truncate_with_an_ellipsis() {
    let s = "a".repeat(40);
    let out = truncate_title(&s, 30);
    assert!(out.ends_with('…'));
    assert!(out.chars().count() <= 30);
}

#[test]
fn short_titles_pass_through() {
    assert_eq!(truncate_title("Disk Saver", 30), "Disk Saver");
}

#[test]
fn pinned_match_is_case_insensitive_prefix() {
    assert!(pinned("Grok Bot"));
    assert!(pinned("grok bot extra"));
    assert!(pinned("Liberado-Budget Manager"));
    assert!(!pinned("Other Agent"));
}

/// The chevron glyph is `display: block`. Without a row flex on the button, the
/// "Show more (N)" label drops onto the next line.
#[test]
fn show_more_label_stays_on_the_chevron_row() {
    let css = include_str!("../../styles/main.css");
    let rule = css_block(css, ".sidebar-show-more");
    assert!(rule.contains("display: flex"), "{rule}");
    assert!(rule.contains("align-items: center"), "{rule}");
    assert!(rule.contains("white-space: nowrap"), "{rule}");
    assert!(
        !rule.contains("flex-direction: column"),
        "a column would put the label under the chevron: {rule}"
    );
    let row = include_str!("agents.rs");
    let show = row.split("fn ShowMoreRow").nth(1).expect("ShowMoreRow");
    let show = show.split("fn AgentTailList").next().unwrap_or(show);
    assert!(show.contains("IconChevronRight"));
    assert!(show.contains("Show more ({count})"));
}

fn css_block<'a>(css: &'a str, selector: &str) -> &'a str {
    let needle = format!("{selector} {{");
    let start = css
        .find(&needle)
        .unwrap_or_else(|| panic!("missing CSS rule {selector}"))
        + needle.len();
    let rest = &css[start..];
    let end = rest
        .find('}')
        .unwrap_or_else(|| panic!("unterminated CSS rule {selector}"));
    &rest[..end]
}

#[test]
fn dot_palette_covers_prototype_categories() {
    for name in [
        "Grok",
        "Liberado-Budget",
        "Disk Saver",
        "Self Help",
        "Life System",
        "Homelab",
    ] {
        assert!(
            !row_dot_color(name).is_empty(),
            "row_dot_color should resolve a colour for {name}"
        );
    }
}
