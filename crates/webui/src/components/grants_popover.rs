//! Read-only Tools popover for the chat header.
//!
//! Lists the agent grant and the session grant from `GET /api/conversations/{id}/grants`.
//! It does not revoke, and it does not touch the transcript.

use dioxus::prelude::*;

use chat_client_contract::{AuthorityGrants, ConversationGrantsResponse, GrantSource};

/// Lines the popover renders for one side. Empty authority is one sentence, so the
/// section never looks like a failed load.
pub(crate) fn grant_lines(grants: &AuthorityGrants) -> Vec<String> {
    if grants.is_empty() {
        return vec!["Nothing granted.".to_string()];
    }
    let mut lines = Vec::new();
    for name in &grants.mcps {
        lines.push(format!("MCP {name}"));
    }
    for name in &grants.tools {
        lines.push(format!("Tool {name}"));
    }
    for zone in &grants.zones {
        lines.push(format!(
            "Zone {} \u{2014} {} ({})",
            zone.name, zone.access, zone.write_class
        ));
    }
    if grants.ask_human {
        lines.push("Ask human".to_string());
    }
    lines
}

pub(crate) fn callable_lines(names: &[String]) -> Vec<String> {
    if names.is_empty() {
        vec!["No connected tool is callable.".to_string()]
    } else {
        names
            .iter()
            .map(|name| format!("Callable {name}"))
            .collect()
    }
}

pub(crate) fn agent_heading(component: &str, in_effect: bool) -> String {
    let base = format!("Agent ({component})");
    if in_effect {
        format!("{base} \u{2014} in effect")
    } else {
        base
    }
}

pub(crate) fn session_heading(profile: Option<&str>, in_effect: bool) -> String {
    let base = match profile {
        Some(name) => format!("Session {name}"),
        None => "Session".to_string(),
    };
    if in_effect {
        format!("{base} \u{2014} in effect")
    } else {
        base
    }
}

pub(crate) fn section_class(in_effect: bool) -> &'static str {
    if in_effect {
        "tools-section in-effect"
    } else {
        "tools-section"
    }
}

/// Why the session section looks the way it does. `None` when the grant lines are enough.
/// Lines under the session heading. An unnamed session inherits the agent grant,
/// so the blurb is the whole section — "Nothing granted." would contradict it.
pub(crate) fn session_lines(profile: Option<&str>, grants: &AuthorityGrants) -> Vec<String> {
    if profile.is_none() {
        Vec::new()
    } else {
        grant_lines(grants)
    }
}

pub(crate) fn session_blurb(
    profile: Option<&str>,
    session_empty: bool,
    session_in_effect: bool,
) -> Option<&'static str> {
    if profile.is_none() {
        return Some("No session profile. The agent grant applies.");
    }
    if session_in_effect && session_empty {
        return Some("This profile grants nothing.");
    }
    None
}

pub(crate) fn source_line(effective: GrantSource, profile: Option<&str>) -> String {
    match (effective, profile) {
        (GrantSource::Session, Some(name)) => {
            format!("Session profile {name} is in effect.")
        }
        (GrantSource::Session, None) => "Session grant is in effect.".to_string(),
        (GrantSource::Agent, _) => "Agent grant is in effect.".to_string(),
    }
}

async fn fetch_grants(api_base: &str, conv_id: &str) -> Result<ConversationGrantsResponse, String> {
    let url = format!("{api_base}/api/conversations/{conv_id}/grants");
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(format!("Grants unavailable ({status})"));
    }
    resp.json().await.map_err(|e| format!("Bad response: {e}"))
}

enum Fetched {
    NoChat,
    Ready(ConversationGrantsResponse),
}

#[component]
fn GrantSection(
    title: String,
    lines: Vec<String>,
    in_effect: bool,
    blurb: Option<String>,
) -> Element {
    let class = section_class(in_effect);
    rsx! {
        section {
            class: "{class}",
            p { class: "tools-section-title", "{title}" }
            if let Some(blurb) = blurb {
                p { class: "tools-note", "{blurb}" }
            }
            if !lines.is_empty() {
                ul {
                    class: "tools-lines",
                    for line in lines {
                        li { "{line}" }
                    }
                }
            }
        }
    }
}

fn ready_body(view: &ConversationGrantsResponse) -> Element {
    let agent_in_effect = view.effective == GrantSource::Agent;
    let session_in_effect = view.effective == GrantSource::Session;
    let blurb = session_blurb(
        view.profile.as_deref(),
        view.session.is_empty(),
        session_in_effect,
    )
    .map(str::to_string);
    let status = source_line(view.effective, view.profile.as_deref());
    let agent_title = agent_heading(&view.agent_component, agent_in_effect);
    let session_title = session_heading(view.profile.as_deref(), session_in_effect);
    let agent_lines = grant_lines(&view.agent);
    let session_rows = session_lines(view.profile.as_deref(), &view.session);
    let callable = callable_lines(&view.callable);
    rsx! {
        p { class: "tools-status", "{status}" }
        GrantSection {
            title: agent_title,
            lines: agent_lines,
            in_effect: agent_in_effect,
            blurb: None,
        }
        GrantSection {
            title: session_title,
            lines: session_rows,
            in_effect: session_in_effect,
            blurb,
        }
        GrantSection {
            title: "Callable now".to_string(),
            lines: callable,
            in_effect: true,
            blurb: None,
        }
    }
}

/// Header control. Owned by `App` so Back can close it. Read-only.
#[component]
pub fn ToolsPopover(
    api_base: String,
    active_conv_id: Signal<Option<String>>,
    mut open: Signal<bool>,
) -> Element {
    let grants = use_resource(move || {
        let base = api_base.clone();
        let id = active_conv_id();
        let shown = open();
        async move {
            if !shown {
                return Ok(Fetched::NoChat);
            }
            let Some(id) = id else {
                return Ok(Fetched::NoChat);
            };
            fetch_grants(&base, &id).await.map(Fetched::Ready)
        }
    });

    let class = if open() {
        "tools-btn open"
    } else {
        "tools-btn"
    };

    rsx! {
        div {
            class: "tools-popover-wrap",
            button {
                class: "{class}",
                r#type: "button",
                title: "Tools this conversation may use",
                onclick: move |_| open.set(!open()),
                "Tools"
            }
            if open() {
                div {
                    class: "tools-popover",
                    match grants.read().as_ref() {
                        None => rsx! { p { class: "tools-status", "Loading..." } },
                        Some(Err(error)) => rsx! { p { class: "tools-status", "{error}" } },
                        Some(Ok(Fetched::NoChat)) => rsx! {
                            p { class: "tools-status", "Open a chat to see its tools." }
                        },
                        Some(Ok(Fetched::Ready(view))) => ready_body(view),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chat_client_contract::ZoneGrant;

    fn sample(effective: GrantSource, profile: Option<&str>) -> ConversationGrantsResponse {
        ConversationGrantsResponse {
            id: "01CONV".into(),
            profile: profile.map(str::to_string),
            effective,
            agent_component: "main-agent".into(),
            agent: AuthorityGrants {
                mcps: vec!["tasks-mcp".into()],
                tools: Vec::new(),
                zones: vec![ZoneGrant {
                    name: "tasks".into(),
                    access: "read".into(),
                    write_class: "proposal_only".into(),
                }],
                ask_human: true,
            },
            session: AuthorityGrants {
                mcps: Vec::new(),
                tools: vec!["turbovault:read_note".into()],
                zones: Vec::new(),
                ask_human: false,
            },
            callable: vec!["tasks-mcp:add".into()],
        }
    }

    #[test]
    fn grant_lines_name_mcp_tool_zone_and_ask_human() {
        let lines = grant_lines(&sample(GrantSource::Agent, None).agent);
        assert_eq!(
            lines,
            vec![
                "MCP tasks-mcp".to_string(),
                "Zone tasks \u{2014} read (proposal_only)".to_string(),
                "Ask human".to_string(),
            ]
        );
    }

    #[test]
    fn an_empty_grant_says_nothing_granted() {
        assert_eq!(
            grant_lines(&AuthorityGrants::default()),
            vec!["Nothing granted.".to_string()]
        );
        assert_eq!(
            callable_lines(&[]),
            vec!["No connected tool is callable.".to_string()]
        );
        assert_eq!(
            callable_lines(&["tasks-mcp:add".into()]),
            vec!["Callable tasks-mcp:add".to_string()]
        );
    }

    #[test]
    fn headings_mark_only_the_side_in_effect() {
        assert_eq!(
            agent_heading("main-agent", true),
            "Agent (main-agent) \u{2014} in effect"
        );
        assert_eq!(agent_heading("main-agent", false), "Agent (main-agent)");
        assert_eq!(
            session_heading(Some("research"), true),
            "Session research \u{2014} in effect"
        );
        assert_eq!(session_heading(None, false), "Session");
        assert_eq!(section_class(true), "tools-section in-effect");
        assert_eq!(section_class(false), "tools-section");
    }

    #[test]
    fn the_session_blurb_explains_inherit_and_an_empty_profile() {
        assert_eq!(
            session_blurb(None, true, false),
            Some("No session profile. The agent grant applies.")
        );
        assert_eq!(
            session_blurb(Some("quiet"), true, true),
            Some("This profile grants nothing.")
        );
        assert_eq!(session_blurb(Some("research"), false, true), None);
        assert!(session_lines(None, &AuthorityGrants::default()).is_empty());
        assert_eq!(
            session_lines(Some("quiet"), &AuthorityGrants::default()),
            vec!["Nothing granted.".to_string()]
        );
        assert_eq!(
            source_line(GrantSource::Agent, None),
            "Agent grant is in effect."
        );
        assert_eq!(
            source_line(GrantSource::Session, Some("research")),
            "Session profile research is in effect."
        );
    }

    #[test]
    fn the_popover_lives_in_the_header_and_not_in_the_transcript() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let app = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
        let chat = std::fs::read_to_string(root.join("src/components/chat.rs")).unwrap();
        let css = std::fs::read_to_string(root.join("src/styles/main.css")).unwrap();
        assert!(app.contains("ToolsPopover"), "{app}");
        assert!(
            !chat.contains("ToolsPopover") && !chat.contains("tools-popover"),
            "the tools list must stay out of the transcript"
        );
        let start = css.find(".tools-popover {").expect("tools popover rule");
        let block = &css[start..css.len().min(start + 500)];
        assert!(
            block.contains("overflow: auto"),
            "the popover scrolls itself, not the transcript: {block}"
        );
    }
}
