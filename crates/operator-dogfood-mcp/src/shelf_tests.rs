use super::*;
use serde_json::json;

fn row(id: &str, title: &str, created_at: &str, mode: &str, profile: &str, creator: bool) -> Value {
    let mut header = json!({
        "id": id,
        "title": title,
        "created_at": created_at,
        "surface_mode": mode,
        "grant": {"profile": profile},
    });
    if creator {
        header["agent_creator"] = json!(true);
    }
    header
}

#[test]
fn identity_trims_and_blank_title_uses_the_profile_name() {
    let (profile, title) = agent_identity("  coding  ", Some("  Budget  ")).unwrap();
    assert_eq!(profile, "coding");
    assert_eq!(title, "Budget");
    let (profile, title) = agent_identity("coding", Some("   ")).unwrap();
    assert_eq!(profile, "coding");
    assert_eq!(title, "coding");
    let err = agent_identity("  ", None).unwrap_err();
    assert!(err.to_string().contains("non-empty"), "{err}");
}

#[test]
fn oldest_match_skips_chat_rows_the_creator_and_a_missing_shelf() {
    let headers = vec![
        row(
            "chat",
            "Budget",
            "2026-01-01T00:00:00Z",
            "chat",
            "coding",
            false,
        ),
        row(
            "creator",
            "Budget",
            "2026-01-01T00:00:00Z",
            "agent",
            "coding",
            true,
        ),
        row(
            "new",
            "Budget",
            "2026-03-01T00:00:00Z",
            "agent",
            "coding",
            false,
        ),
        row(
            "old",
            "Budget",
            "2026-02-01T00:00:00Z",
            "agent",
            "coding",
            false,
        ),
        row(
            "other",
            "Notes",
            "2026-01-01T00:00:00Z",
            "agent",
            "coding",
            false,
        ),
        json!({"id": "bare", "title": "Budget", "grant": {"profile": "coding"}}),
    ];
    let found = oldest_match(&headers, "coding", "Budget").unwrap();
    assert_eq!(found["id"], "old");
}

#[test]
fn same_timestamp_breaks_ties_by_id() {
    let headers = vec![
        row(
            "01B",
            "Budget",
            "2026-02-01T00:00:00Z",
            "agent",
            "coding",
            false,
        ),
        row(
            "01A",
            "Budget",
            "2026-02-01T00:00:00Z",
            "agent",
            "coding",
            false,
        ),
    ];
    let found = oldest_match(&headers, "coding", "Budget").unwrap();
    assert_eq!(found["id"], "01A");
}

#[test]
fn auto_si_fractional_digits_are_not_chronological_as_wire_text() {
    let older = chrono::DateTime::parse_from_rfc3339("2026-02-01T00:00:00.100Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let newer = chrono::DateTime::parse_from_rfc3339("2026-02-01T00:00:00.100001Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let older_text = serde_json::to_value(older).unwrap();
    let newer_text = serde_json::to_value(newer).unwrap();
    assert_eq!(older_text, json!("2026-02-01T00:00:00.100Z"));
    assert_eq!(newer_text, json!("2026-02-01T00:00:00.100001Z"));
    assert!(
        newer_text.as_str().unwrap() < older_text.as_str().unwrap(),
        "AutoSi wire text must be the case that string-min would pick the later row"
    );
    assert!(older < newer);
}

#[test]
fn oldest_match_uses_parsed_created_at_not_auto_si_wire_text() {
    let headers = vec![
        row(
            "newer",
            "Budget",
            "2026-02-01T00:00:00.100001Z",
            "agent",
            "coding",
            false,
        ),
        row(
            "older",
            "Budget",
            "2026-02-01T00:00:00.100Z",
            "agent",
            "coding",
            false,
        ),
    ];
    let found = oldest_match(&headers, "coding", "Budget").unwrap();
    assert_eq!(found["id"], "older");
}

#[test]
fn unparseable_created_at_loses_to_a_parseable_timestamp() {
    let headers = vec![
        json!({
            "id": "aaa",
            "title": "Budget",
            "created_at": "not-a-timestamp",
            "surface_mode": "agent",
            "grant": {"profile": "coding"},
        }),
        row(
            "zzz",
            "Budget",
            "2026-02-01T00:00:00Z",
            "agent",
            "coding",
            false,
        ),
        json!({
            "id": "bbb",
            "title": "Budget",
            "surface_mode": "agent",
            "grant": {"profile": "coding"},
        }),
    ];
    let found = oldest_match(&headers, "coding", "Budget").unwrap();
    assert_eq!(found["id"], "zzz");
}

#[test]
fn unparseable_created_at_rows_break_ties_by_id() {
    let headers = vec![
        json!({
            "id": "m",
            "title": "Budget",
            "created_at": "nope",
            "surface_mode": "agent",
            "grant": {"profile": "coding"},
        }),
        json!({
            "id": "a",
            "title": "Budget",
            "surface_mode": "agent",
            "grant": {"profile": "coding"},
        }),
    ];
    let found = oldest_match(&headers, "coding", "Budget").unwrap();
    assert_eq!(found["id"], "a");
}

#[test]
fn eligibility_is_true_only_for_an_explicit_flag() {
    let body = json!({
        "profiles": [
            {"name": "coding", "agent_eligible": true},
            {"name": "chat-default", "agent_eligible": false},
            {"name": "quiet"}
        ]
    });
    assert!(profile_eligibility(&body, "coding").unwrap());
    assert!(!profile_eligibility(&body, "chat-default").unwrap());
    assert!(!profile_eligibility(&body, "quiet").unwrap());
    let missing = profile_eligibility(&body, "nope").unwrap_err();
    assert!(missing.to_string().contains("not an enabled"), "{missing}");
    let bad = profile_eligibility(&json!({}), "coding").unwrap_err();
    assert!(bad.to_string().contains("profiles array"), "{bad}");
}

#[test]
fn latest_assistant_is_the_last_one_and_blank_human_text_is_refused() {
    let history = json!({
        "messages": [
            {"role": "system", "content": "prompt"},
            {"role": "assistant", "content": "old"},
            {"role": "user", "content": "hello"},
            {"role": "assistant", "content": "newer reply"}
        ]
    });
    assert_eq!(latest_assistant(&history).unwrap(), "newer reply");
    assert!(latest_assistant(&json!({"messages": []})).is_none());
    let err = human_message("   ").unwrap_err();
    assert!(err.to_string().contains("does not invent"), "{err}");
    let err = session_id("bad/id").unwrap_err();
    assert!(err.to_string().contains("path"), "{err}");
}

#[test]
fn workspace_report_accepts_a_prefixed_tool_name() {
    let report = workspace_report(&[
        "face:workspace_read".into(),
        "workspace_list".into(),
        "workspace_reader".into(),
    ]);
    assert_eq!(
        report["present"],
        json!(["workspace_list", "workspace_read"])
    );
    assert_eq!(
        report["missing"],
        json!(["workspace_write", "workspace_delete", "workspace_download"])
    );
    assert_eq!(report["read_only"], true);
}

#[test]
fn session_rows_keep_goal_and_profile() {
    let chat = slim_session(&json!({
        "id": "c",
        "title": "Hi",
        "surface_mode": "chat",
        "status": "running"
    }));
    assert_eq!(chat["has_goal"], false);
    assert_eq!(chat["profile"], Value::Null);
    let goal = slim_session(&json!({
        "id": "g",
        "goal": {"description": "ship"},
        "grant": {"profile": "coding"},
        "agent_creator": true
    }));
    assert_eq!(goal["has_goal"], true);
    assert_eq!(goal["profile"], "coding");
    assert_eq!(goal["agent_creator"], true);
    assert!(is_shelf_agent(&row(
        "a",
        "Budget",
        "2026-01-01T00:00:00Z",
        "agent",
        "coding",
        false
    )));
    assert!(!is_shelf_agent(&row(
        "a",
        "Budget",
        "2026-01-01T00:00:00Z",
        "agent",
        "coding",
        true
    )));
}
