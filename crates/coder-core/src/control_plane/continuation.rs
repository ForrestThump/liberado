//! Continuation prompts synthesized from a task projection, not from chat replay.

use super::TaskRecord;

/// Synthesizes structured markdown prompts for continuation across worker boundaries.
pub struct ContinuationContextBuilder;

impl ContinuationContextBuilder {
    /// Synthesize a normalized continuation prompt from a task projection.
    pub fn build(record: &TaskRecord) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "You are continuing work on task `{}`.\n\n",
            record.task_id
        ));

        out.push_str("## Objective\n");
        out.push_str(&format!("{}\n\n", record.objective.trim()));

        if !record.acceptance_criteria.is_empty() {
            out.push_str("## Acceptance Criteria\n");
            for ac in &record.acceptance_criteria {
                out.push_str(&format!("- {}\n", ac.trim()));
            }
            out.push('\n');
        }

        out.push_str("## Worktree State\n");
        out.push_str(&format!("- Worktree: `{}`\n", record.worktree));
        out.push_str(&format!("- Branch: `{}`\n", record.branch));
        out.push_str(&format!("- Base ref: `{}`\n", record.base_ref));
        if let Some(sha) = &record.head_revision {
            out.push_str(&format!("- Head revision: `{sha}`\n"));
        }

        if !record.commits.is_empty() {
            out.push_str("- Existing commits on branch:\n");
            for sha in &record.commits {
                out.push_str(&format!("  - `{sha}`\n"));
            }
        }
        out.push('\n');

        if !record.failures.is_empty() {
            out.push_str("## Failures\n");
            out.push_str("The following checks or tests failed:\n");
            for f in &record.failures {
                out.push_str(&format!("- `{f}`\n"));
            }
            out.push('\n');
        }

        if let Some(excerpt) = &record.latest_failure_excerpt {
            out.push_str("### Failure Log Excerpt\n```text\n");
            out.push_str(excerpt.trim());
            out.push_str("\n```\n\n");
        }

        if let Some(diagnosis) = &record.current_diagnosis {
            out.push_str("## Review Diagnosis\n");
            out.push_str(&format!("{}\n\n", diagnosis.trim()));
        }

        out.push_str("## Instructions\n");
        out.push_str("1. Reproduce any reported failure in the worktree before modifying code.\n");
        out.push_str("2. Address the defect directly without refactoring unrelated modules.\n");
        out.push_str("3. Verify that the acceptance criteria are met and tests pass.\n");
        out.push_str("4. Commit your changes with a clear description.\n");

        out
    }
}
