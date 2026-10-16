//! Sidebar helper tests.
//!
//! Split from `sidebar.rs` for module-health boundaries.

use super::agents::{pinned, row_dot_color, truncate_title};
use super::{
    delete_accepted, deleted_the_open_conversation, page_from_list, relative_time, rename_accepted,
    rename_body,
};

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
fn rename_accepts_only_2xx() {
    for ok in [200, 204, 299] {
        assert!(rename_accepted(ok), "{ok} should rename cleanly");
    }
    for no in [404, 400, 500] {
        assert!(
            !rename_accepted(no),
            "{no} should surface as a rename failure"
        );
    }
}

#[test]
fn rename_body_sends_only_the_title() {
    let body = rename_body("  Notes");
    assert_eq!(body["title"], "  Notes");
    assert_eq!(body.as_object().map(|map| map.len()), Some(1));
}

#[test]
fn deleting_the_open_conversation_starts_a_fresh_chat() {
    assert!(deleted_the_open_conversation(Some("open"), "open"));
    assert!(!deleted_the_open_conversation(Some("open"), "other"));
    assert!(!deleted_the_open_conversation(None, "open"));
    let sidebar = include_str!("mod.rs");
    let success = window_around(sidebar, "if deleted_the_open_conversation", 450);
    assert!(success.contains("Route::Chat"));
    assert!(success.contains("new_chat_nonce += 1"));
    assert!(success.contains("close_phone_drawer"));
}

#[test]
fn the_list_keeps_the_built_in_creator_id() {
    let raw = r#"[
        {"id":"01","title":"Agent Creator","created_at":"2026-01-01T00:00:00Z","agent_creator":true,"surface_mode":"agent","grant":{"profile":"operator"}},
        {"id":"02","title":"Disk Saver","created_at":"2026-01-01T00:00:00Z","surface_mode":"agent"}
    ]"#;
    let page = page_from_list(&serde_json::from_str(raw).unwrap()).unwrap();
    assert_eq!(page.creator_ids, vec!["01".to_string()]);
    assert_eq!(page.headers.len(), 2);
    assert_eq!(page.headers[1].title.as_deref(), Some("Disk Saver"));
    assert_eq!(
        page.profiles.get("01").map(String::as_str),
        Some("operator")
    );
    assert!(!page.profiles.contains_key("02"));
    assert!(page.editor_ids.is_empty());
    assert!(page_from_list(&serde_json::json!({"profiles": []})).is_err());
}

#[test]
fn the_list_keeps_the_automation_editor_id_off_the_creator_set() {
    let raw = r#"[
        {"id":"01","title":"Automation Editor","created_at":"2026-01-01T00:00:00Z","automation_editor":true,"surface_mode":"chat"},
        {"id":"02","title":"Notes","created_at":"2026-01-01T00:00:00Z","surface_mode":"chat"}
    ]"#;
    let page = page_from_list(&serde_json::from_str(raw).unwrap()).unwrap();
    assert_eq!(page.editor_ids, vec!["01".to_string()]);
    assert!(page.creator_ids.is_empty());
    assert_eq!(page.headers.len(), 2);
}

/// Agent rows rename and delete the profile. Chat rows still rename the conversation.
#[test]
fn agent_rows_call_the_profile_routes() {
    let agents = include_str!("agents.rs");
    let compact = agents
        .split("fn SidebarRowCompact")
        .nth(1)
        .expect("SidebarRowCompact");
    let compact = compact.split("fn SidebarRowChat").next().unwrap_or(compact);
    assert!(compact.contains("noun: \"agent\""));
    assert!(compact.contains("profile_name"));
    assert!(compact.contains("delete_block"));
    let sidebar = include_str!("mod.rs");
    assert!(sidebar.contains("/api/conversations/{id}"));
    let profile_api = include_str!("profile_api.rs");
    assert!(profile_api.contains("/api/profiles/{name}"));
    assert!(profile_api.contains("Method::PATCH"));
    assert!(profile_api.contains("Method::DELETE"));
    assert!(sidebar.contains("rename_profile"));
    assert!(sidebar.contains("delete_profile"));
}

/// The options control is a real button on every chat row, including on a phone.
#[test]
fn chat_rows_expose_a_tappable_options_button() {
    let row = include_str!("agents.rs");
    let chat = row
        .split("fn SidebarRowChat")
        .nth(1)
        .expect("SidebarRowChat");
    let chat = chat.split("fn PinnedList").next().unwrap_or(chat);
    assert!(chat.contains("RowOptions"));
    assert!(chat.contains("noun: \"chat\""));
    let css = include_str!("../../styles/main.css");
    let rule = css_block(css, ".grok-row-options");
    assert!(rule.contains("display: inline-flex"), "{rule}");
    assert!(!rule.contains("display: none"), "{rule}");
    assert!(!rule.contains("opacity: 0"), "{rule}");
    let phone = css
        .split("/* phone-sidebar-drawer-end */")
        .next()
        .expect("phone drawer styles");
    assert!(
        phone.contains(".grok-row-options"),
        "the phone tap target must include the options button"
    );
    assert!(phone.contains("min-height: 2.5rem"));
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

/// The projects `+` used to open a Create dialog that only closed. Projects are
/// declared in topology.toml, so the sidebar must not offer that action.
#[test]
fn the_projects_section_does_not_offer_create() {
    let sidebar = include_str!("mod.rs");
    assert!(!sidebar.contains("Create project"));
    assert!(!sidebar.contains("ProjectsDialog"));
    assert!(!sidebar.contains("on_create"));
    assert!(sidebar.contains("sidebar-section-label\", \"Projects\""));
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

/// Chat, Manage plugins, and the other view links must close the phone drawer.
/// A project row already does this through `collapsed_after_pick`.
#[test]
fn nav_that_opens_a_view_closes_the_phone_drawer() {
    let sidebar = include_str!("mod.rs");
    let rail = include_str!("rail.rs");
    let projects = include_str!("projects.rs");
    let primary = rail
        .split("fn PrimaryNavRow")
        .nth(1)
        .expect("PrimaryNavRow");
    let primary = primary.split("fn ChatBubble").next().unwrap_or(primary);
    assert!(
        primary.contains("close_phone_drawer"),
        "Chat, Library, and Automations share PrimaryNavRow: {primary}"
    );
    for target in ["Route::Chat", "Route::Library", "Route::Automations"] {
        assert!(
            sidebar.contains(&format!("collapsed: collapsed, target: {target}")),
            "{target} nav must receive the drawer signal"
        );
    }
    let plugins = projects.split("fn PluginList").nth(1).expect("PluginList");
    assert!(
        plugins.contains("close_phone_drawer"),
        "Manage plugins must close the drawer: {plugins}"
    );
    for marker in [
        "\"aria-label\": \"Liberado home\"",
        "sidebar-section-label\", \"Agents\"",
        "sidebar-section-label\", \"Projects\"",
        "sidebar-section-label\", \"Plugins\"",
    ] {
        assert!(
            window_around(sidebar, marker, 500).contains("close_phone_drawer"),
            "{marker} must close the drawer"
        );
    }
}

/// The theme list hangs into the chat. The rail must paint above the composer,
/// and the list must sit outside `.sidebar-scroll`, which clips both axes.
/// Rename and delete menus are already `position: fixed` at the overlay layer,
/// so the same clip does not apply to them.
#[test]
fn theme_picker_paints_above_the_chat_and_is_not_clipped() {
    let css = include_str!("../../styles/main.css");
    let sidebar = css_block(css, ".grok-layout .sidebar");
    let composer = css_z_index(css_block(css, ".composer-dock"));
    let header = css_z_index(css_block(css, ".app-header"));
    let modal = css_z_index(css_block(css, ".modal-backdrop"));
    let rail_z = css_z_index(sidebar);
    assert!(
        sidebar.contains("position: relative"),
        "the rail needs a stacking context: {sidebar}"
    );
    assert!(
        sidebar.contains("overflow: visible"),
        "hidden overflow clips the theme list at the rail edge: {sidebar}"
    );
    assert!(
        rail_z > composer,
        "rail z-index {rail_z} must be above the composer {composer}"
    );
    assert!(
        header > rail_z,
        "the header ({header}) stays above the rail ({rail_z})"
    );
    assert!(
        modal > rail_z,
        "modals ({modal}) stay above the rail ({rail_z})"
    );

    let pop = css_block(css, ".theme-swatch-pop");
    assert!(
        pop.contains("position: absolute"),
        "the desktop list stays anchored to the palette button: {pop}"
    );
    assert!(css_z_index(pop) > 0, "{pop}");

    let phone_drawer = css
        .split("/* phone-sidebar-drawer */")
        .nth(1)
        .expect("phone drawer styles")
        .split("/* phone-sidebar-drawer-end */")
        .next()
        .expect("phone drawer end");
    let drawer = css_block(phone_drawer, ".grok-layout.drawer-open .sidebar");
    assert!(drawer.contains("position: fixed"), "{drawer}");
    assert!(drawer.contains("z-index: 25"), "{drawer}");
    assert!(
        drawer.contains("overflow: hidden"),
        "the phone overlay stays clipped: {drawer}"
    );
    assert!(
        css_z_index(css_block(phone_drawer, ".app-header")) > css_z_index(drawer),
        "the phone header stays above the drawer"
    );

    let phone_pop = nth_css_block(css, ".theme-swatch-pop", 1);
    assert!(
        phone_pop.contains("position: fixed"),
        "a phone keeps the centered fixed list: {phone_pop}"
    );
    assert!(
        phone_pop.contains("transform: translateX(-50%)"),
        "{phone_pop}"
    );

    let menu = css_block(css, ".conv-menu-backdrop");
    assert!(menu.contains("position: fixed"), "{menu}");
    assert!(
        css_z_index(menu) >= modal,
        "row menus stay on the overlay layer: {menu}"
    );

    let src = include_str!("mod.rs");
    let scroll_at = src
        .find("class: \"sidebar-scroll\"")
        .expect("sidebar scroll");
    let div_at = src[..scroll_at].rfind("div {").expect("scroll div");
    let scroll_end = element_end(src, div_at);
    let swatch_at = src
        .rfind("ThemeSwatch { theme_epoch: theme_epoch }")
        .expect("expanded theme swatch");
    assert!(
        swatch_at > scroll_end,
        "ThemeSwatch must be outside .sidebar-scroll or the list clips the picker"
    );

    let rail = include_str!("rail.rs");
    let swatch = rail.split("fn ThemeSwatch").nth(1).expect("ThemeSwatch");
    let swatch = swatch.split("fn theme_dot").next().unwrap_or(swatch);
    assert!(swatch.contains("open.set(!open())"), "{swatch}");
    assert!(swatch.contains("open.set(false)"), "{swatch}");
}

/// A collapsed desk rail is an icon column. The open rail stays 260px.
/// The phone drawer still hides the collapsed rail.
#[test]
fn collapsed_desktop_rail_is_a_narrow_icon_column() {
    let css = include_str!("../../styles/main.css");
    let open = css_block(css, ".grok-layout .sidebar");
    assert!(
        open.contains("width: 260px"),
        "the expanded rail stays wide: {open}"
    );

    let marker = "/* desktop-collapsed-rail */";
    let at = css.find(marker).expect("desktop collapsed rail marker");
    let after = css[at + marker.len()..].trim_start();
    assert!(
        after.starts_with("@media (min-width: 769px)"),
        "the narrow width must stay inside the desk query so the phone drawer still wins: {after}"
    );
    let rule = css_block(after, ".grok-layout .sidebar.sidebar-collapsed");
    assert!(rule.contains("width: 44px"), "{rule}");
    assert!(rule.contains("max-width: 44px"), "{rule}");

    let phone = css
        .split("/* phone-sidebar-drawer */")
        .nth(1)
        .expect("phone drawer styles")
        .split("/* phone-sidebar-drawer-end */")
        .next()
        .expect("phone drawer end");
    let hidden = css_block(phone, ".grok-layout .sidebar.sidebar-collapsed");
    assert!(hidden.contains("display: none"), "{hidden}");
    assert!(hidden.contains("width: 0"), "{hidden}");
}

fn css_z_index(block: &str) -> i32 {
    let line = block
        .lines()
        .find(|line| line.contains("z-index:"))
        .unwrap_or_else(|| panic!("no z-index in {block}"));
    let digits: String = line.chars().filter(|c| c.is_ascii_digit()).collect();
    digits
        .parse()
        .unwrap_or_else(|_| panic!("bad z-index in {line}"))
}

fn nth_css_block<'a>(css: &'a str, selector: &str, n: usize) -> &'a str {
    let needle = format!("{selector} {{");
    let mut rest = css;
    let mut found = "";
    for _ in 0..=n {
        let start = rest
            .find(&needle)
            .unwrap_or_else(|| panic!("missing CSS rule {selector}"))
            + needle.len();
        let end = rest[start..]
            .find('}')
            .unwrap_or_else(|| panic!("unterminated CSS rule {selector}"));
        found = &rest[start..start + end];
        rest = &rest[start + end..];
    }
    found
}

/// Index of the `}` that closes the element whose `{` is at `open_brace`.
/// Quoted strings and `//` comments do not count.
fn element_end(src: &str, open_brace: usize) -> usize {
    let bytes = src.as_bytes();
    let mut i = open_brace;
    let mut depth = 0i32;
    let mut line_comment = false;
    let mut in_str = false;
    while i < bytes.len() {
        let c = bytes[i];
        if line_comment {
            if c == b'\n' {
                line_comment = false;
            }
            i += 1;
            continue;
        }
        if in_str {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            line_comment = true;
            i += 2;
            continue;
        }
        if c == b'"' {
            in_str = true;
            i += 1;
            continue;
        }
        if c == b'{' {
            depth += 1;
        }
        if c == b'}' {
            depth -= 1;
            if depth == 0 {
                return i;
            }
        }
        i += 1;
    }
    panic!("unclosed element at {open_brace}");
}

fn window_around<'a>(src: &'a str, marker: &str, span: usize) -> &'a str {
    let start = src
        .find(marker)
        .unwrap_or_else(|| panic!("missing {marker}"));
    let from = start.saturating_sub(span);
    let to = (start + marker.len() + span).min(src.len());
    &src[from..to]
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
