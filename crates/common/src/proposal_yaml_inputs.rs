use super::*;

#[test]
fn non_ascii_rationale_round_trips_through_yaml() {
    let mut p = sample_proposal();
    p.rationale = "café weekly — naïve".into();
    let back = Proposal::from_note(&p.to_note()).unwrap();
    assert_eq!(back.rationale, p.rationale);
}

#[test]
fn parses_hand_written_proposal_frontmatter() {
    use crate::capability::Zone;
    let note = r#"---
id: prop-1
correlation_id: corr-1
source: liberado
proposed_action: !ToolCalls
- tool: email:send
  args:
    to: boss@example.com
rationale: café weekly
status: pending
created: 2026-01-02T03:04:05Z
pool: default
requested_grant: '{"Write":{"Vault":"finance"}}'
approved_scope: once
integrity: ''
---

# Proposal: prop-1
"#;
    let parsed = Proposal::from_note(note).unwrap();
    assert_eq!(parsed.id, "prop-1");
    assert_eq!(parsed.correlation_id, "corr-1");
    assert_eq!(parsed.source, "liberado");
    assert_eq!(parsed.rationale, "café weekly");
    assert_eq!(parsed.status, ProposalStatus::Pending);
    assert_eq!(parsed.pool.as_deref(), Some("default"));
    assert_eq!(parsed.approved_scope, Some(GrantScope::Once));
    assert_eq!(parsed.integrity, "");
    assert_eq!(
        parsed.requested_grant,
        Some(Capability::Write(Zone::vault("finance")))
    );
    match parsed.proposed_action {
        ProposedAction::ToolCalls(calls) => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].tool, "email:send");
            assert_eq!(calls[0].args["to"], "boss@example.com");
        }
        other => panic!("expected tool calls, got {other:?}"),
    }
    assert_eq!(
        parsed.created,
        DateTime::parse_from_rfc3339("2026-01-02T03:04:05Z")
            .unwrap()
            .with_timezone(&Utc)
    );
}

#[test]
fn parses_a_proposal_note_without_later_fields() {
    let note = r#"---
id: prop-old
correlation_id: corr-old
source: hook
proposed_action: !External
  description: send the letter
rationale: |
  line one
  line two
status: approved
created: 2026-01-02T03:04:05Z
---

body text
"#;
    let parsed = Proposal::from_note(note).unwrap();
    assert_eq!(parsed.status, ProposalStatus::Approved);
    assert_eq!(parsed.pool, None);
    assert_eq!(parsed.requested_grant, None);
    assert_eq!(parsed.approved_scope, None);
    assert_eq!(parsed.integrity, "");
    assert_eq!(parsed.rationale, "line one\nline two\n");
    assert_eq!(
        parsed.proposed_action,
        ProposedAction::External {
            description: "send the letter".into(),
        }
    );
}

#[test]
fn parses_crlf_proposal_frontmatter() {
    let note = "---\r\nid: prop-crlf\r\ncorrelation_id: c\r\nsource: s\r\nproposed_action: !VaultWrite\r\n  path: finance/x.md\r\n  content_summary: quarter totals\r\nrationale: ok\r\nstatus: rejected\r\ncreated: 2026-01-02T03:04:05Z\r\n---\r\n\r\nbody\r\n";
    let parsed = Proposal::from_note(note).unwrap();
    assert_eq!(parsed.id, "prop-crlf");
    assert_eq!(parsed.status, ProposalStatus::Rejected);
    assert_eq!(
        parsed.proposed_action,
        ProposedAction::VaultWrite {
            path: "finance/x.md".into(),
            content_summary: "quarter totals".into(),
        }
    );
}

#[test]
fn malformed_proposal_frontmatter_is_a_yaml_error() {
    let err = Proposal::from_note("---\nid: [\n---\n").unwrap_err();
    assert!(matches!(err, ProposalNoteError::Yaml(_)), "{err}");
}

#[test]
fn proposal_without_frontmatter_is_rejected() {
    let err = Proposal::from_note("just a body").unwrap_err();
    assert!(matches!(err, ProposalNoteError::MissingFrontmatter));
}
