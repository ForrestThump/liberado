//! Tests for the review/kickback/merge commands. Sibling file per the split
//! pattern; every module reaches the command code through its full path.

mod merge_guard {
    use std::sync::Arc;

    use crate::delegate_cmd::review_cmd::coldreview::verify_required_checks;
    use liberado_forge::{
        CheckState, CheckStates, ForgeClient, ForgeError, MergeCommit, MergeMethod, OpenPr, PrRef,
        RepoPath,
    };

    struct StubForge {
        overall: CheckState,
        diff: Arc<String>,
    }

    #[async_trait::async_trait]
    impl ForgeClient for StubForge {
        async fn open_pr(&self, _req: &OpenPr) -> Result<PrRef, ForgeError> {
            Err(ForgeError::Shape("unused".into()))
        }
        async fn comment(&self, _pr: &PrRef, _body: &str) -> Result<(), ForgeError> {
            Ok(())
        }
        async fn checks(&self, _pr: &PrRef, names: &[String]) -> Result<CheckStates, ForgeError> {
            Ok(CheckStates {
                overall: self.overall,
                named: names.iter().map(|n| (n.clone(), self.overall)).collect(),
            })
        }
        async fn merge(&self, _pr: &PrRef, _m: MergeMethod) -> Result<MergeCommit, ForgeError> {
            Ok(MergeCommit { sha: "abc".into() })
        }
        async fn diff(&self, _pr: &PrRef) -> Result<String, ForgeError> {
            Ok(self.diff.clone().to_string())
        }
    }

    fn pr() -> PrRef {
        PrRef {
            repo: RepoPath("o/r".into()),
            number: 1,
            url: "http://f/o/r/pulls/1".into(),
        }
    }

    /// The D3 gate: a delegator-side refusal when required checks are red.
    #[tokio::test]
    async fn failing_required_checks_block_the_merge_with_names() {
        let forge = StubForge {
            overall: CheckState::Failure,
            diff: Arc::new(String::new()),
        };
        let error = verify_required_checks(
            &forge,
            &pr(),
            &["ci/linux".to_string(), "ci/windows".to_string()],
        )
        .await
        .expect_err("red checks must refuse");
        assert!(error.contains("not green"), "{error}");
        assert!(error.contains("ci/linux"), "{error}");
    }

    #[tokio::test]
    async fn green_checks_and_empty_requirements_pass() {
        let green = StubForge {
            overall: CheckState::Success,
            diff: Arc::new(String::new()),
        };
        verify_required_checks(&green, &pr(), &["ci".to_string()])
            .await
            .expect("green passes");
        // No requirements at all: nothing to verify, no forge call needed.
        let red = StubForge {
            overall: CheckState::Failure,
            diff: Arc::new(String::new()),
        };
        verify_required_checks(&red, &pr(), &[])
            .await
            .expect("empty passes");
    }
}

mod review {
    use crate::delegate_cmd::review_cmd::coldreview::{
        parse_findings, report as review_pr, verdict_summary,
    };
    use liberado_coder_agent::Severity;
    use liberado_delegate_contract::{
        Acceptance, TaskBudget, TaskGrant, TaskId, TaskRecord, TaskSpec, TaskStatus,
    };
    use liberado_forge::{ForgeClient, ForgeError, MergeCommit, MergeMethod, OpenPr, PrRef};
    use liberado_provider::MockProvider;
    use std::sync::Arc;

    struct DiffForge {
        diff: Arc<String>,
        comments: Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl ForgeClient for DiffForge {
        async fn open_pr(&self, _req: &OpenPr) -> Result<PrRef, ForgeError> {
            Err(ForgeError::Shape("unused".into()))
        }
        async fn comment(&self, _pr: &PrRef, body: &str) -> Result<(), ForgeError> {
            self.comments.lock().unwrap().push(body.to_string());
            Ok(())
        }
        async fn checks(
            &self,
            _pr: &PrRef,
            names: &[String],
        ) -> Result<liberado_forge::CheckStates, ForgeError> {
            Ok(liberado_forge::CheckStates {
                overall: liberado_forge::CheckState::Success,
                named: names
                    .iter()
                    .map(|n| (n.clone(), liberado_forge::CheckState::Success))
                    .collect(),
            })
        }
        async fn merge(&self, _pr: &PrRef, _m: MergeMethod) -> Result<MergeCommit, ForgeError> {
            Ok(MergeCommit { sha: "abc".into() })
        }
        async fn diff(&self, _pr: &PrRef) -> Result<String, ForgeError> {
            Ok(self.diff.clone().to_string())
        }
    }

    fn record() -> TaskRecord {
        let spec = TaskSpec {
            id: TaskId("01REVIEW00000000000000TEST".into()),
            project: "p".into(),
            repository: "o/r".into(),
            base_branch: "main".into(),
            goal: "g".into(),
            success_criteria: vec![],
            acceptance: Acceptance::default(),
            budget: TaskBudget::default(),
            grant: TaskGrant::default(),
        };
        TaskRecord {
            spec,
            status: TaskStatus::PrOpened {
                url: "http://forge/o/r/pulls/3".into(),
            },
            session_id: None,
            pr_url: None,
            updated_at: String::new(),
        }
    }

    fn finding_response(body: &str) -> liberado_provider::CompletionResponse {
        liberado_provider::CompletionResponse {
            content: Some(body.into()),
            tool_calls: vec![],
            finish_reason: liberado_provider::FinishReason::Stop,
            usage: None,
        }
    }

    /// The full glue: forge diff -> cold-review request -> mock reviewer -> filtered
    /// verdict. A finding citing a path in the diff must survive the filter.
    #[tokio::test]
    async fn a_code_cited_finding_survives_and_drives_the_verdict() {
        let diff = "diff --git a/kickback-proof.md b/kickback-proof.md\n--- a/kickback-proof.md\n+++ b/kickback-proof.md\n@@ -1 +1 @@\n-version one\n+version two\n";
        let forge = DiffForge {
            diff: Arc::new(diff.into()),
            comments: Arc::default(),
        };
        let reviewer = MockProvider::with_script(
            "reviewer",
            [finding_response(
                r#"{"findings":[{"severity":"high","title":"wrong constant","why":"the string contradicts the goal","path":"kickback-proof.md","location":"L1"}]}"#,
            )],
        );
        let report = review_pr(&forge, &reviewer, &record())
            .await
            .expect("reviews");
        assert!(report.contains("KICK BACK"), "{report}");
        assert!(report.contains("wrong constant"), "{report}");
        assert!(report.contains("(kickback-proof.md)"), "{report}");
    }

    #[tokio::test]
    async fn an_empty_diff_is_refused_before_any_model_call() {
        let forge = DiffForge {
            diff: Arc::new(String::new()),
            comments: Arc::default(),
        };
        let reviewer = MockProvider::new("reviewer");
        let error = review_pr(&forge, &reviewer, &record())
            .await
            .expect_err("empty diff cannot be reviewed");
        assert!(error.contains("non-empty diff"), "{error}");
    }

    #[tokio::test]
    async fn findings_must_be_a_json_object_or_the_error_is_honest() {
        let error = parse_findings("I looked at it and it seems fine").expect_err("prose refused");
        assert!(error.contains("no JSON object"), "{error}");
        let ok = parse_findings(r#"prefix {"findings":[]} suffix"#).unwrap();
        assert!(ok.is_empty());
        // Severity round-trips through serde for the wire shape reviewers emit.
        let parsed =
            parse_findings(r#"{"findings":[{"severity":"low","title":"t","why":"w"}]}"#).unwrap();
        assert_eq!(parsed[0].severity, Severity::Low);
    }

    #[tokio::test]
    async fn the_pr_comment_carries_the_verdict_without_the_header_line() {
        let summary = verdict_summary("Cold review of http://x\n\nNo findings. Verdict: ready.\n");
        assert!(!summary.contains("Cold review of"), "{summary}");
        assert!(summary.contains("ready"), "{summary}");
    }
}
mod grammar {
    use crate::delegate_cmd::review_cmd::coldreview::{
        parse_kickback_args, parse_merge_args, pr_ref_from_url,
    };
    use liberado_forge::{MergeMethod, RepoPath};

    #[test]
    fn kickback_grammar_splits_flags_from_positionals() {
        let (positionals, flags) = parse_kickback_args(
            ["01T", "--body", "fix it", "--comment"]
                .iter()
                .map(|s| s.to_string()),
        )
        .expect("parse");
        assert_eq!(positionals, vec!["01T".to_string()]);
        assert_eq!(flags.body, "fix it");
        assert!(flags.comment);
    }

    #[test]
    fn merge_method_defaults_to_squash_and_parses_the_three_verbs() {
        let (_, flags) = parse_merge_args(["01T"].iter().map(|s| s.to_string())).expect("parse");
        assert_eq!(flags.method, MergeMethod::Squash);
        let (_, flags) =
            parse_merge_args(["01T", "--method", "rebase"].iter().map(|s| s.to_string()))
                .expect("parse");
        assert_eq!(flags.method, MergeMethod::Rebase);
        assert!(parse_merge_args(["--method", "wizard"].iter().map(|s| s.to_string())).is_err());
    }

    #[test]
    fn pr_urls_parse_into_references_like_the_worker_does() {
        let pr = pr_ref_from_url("https://gitea.example/o/r/pulls/9", RepoPath("o/r".into()))
            .expect("parses");
        assert_eq!(pr.number, 9);
        assert!(pr_ref_from_url("https://gitea.example/o/r", RepoPath("o/r".into())).is_err());
    }
}
