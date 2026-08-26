//! The question protocol (plan §8): `ask_delegator`, the answer mailbox, and the
//! per-task backend that carries them into a coding run.
//!
//! The park is an in-memory await, not a persisted pause: the tool's `invoke` blocks on
//! the mailbox until the delegator answers or the timeout falls back to
//! `default_option`. Zero busy-wait, and the executor conversation stays exactly where
//! it was — the answer comes back *in-band* as the tool result, so the model continues
//! the same turn. A worker restart while parked fails the run honestly through the same
//! rescan path as any other mid-run crash; durable session-state resume stays future
//! work until measured to be worth its weight.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use liberado_coder_agent::extension::RuntimeExtension;
use liberado_delegate_contract::{Answer, Question, QuestionOption, TaskId};
use liberado_provider::{ToolDef, ToolInvocation};
use serde_json::json;

/// The tool's wire name. Offered only on delegated runs, where a delegator exists to
/// answer.
pub const ASK_DELEGATOR: &str = "ask_delegator";

/// Where answers find their waiters. One slot per question id; the ask registers
/// before the question goes on the stream, so an instant answer cannot fall in the gap.
#[derive(Default)]
pub struct AnswerMailbox {
    waiters: Mutex<HashMap<String, tokio::sync::oneshot::Sender<Answer>>>,
}

impl AnswerMailbox {
    /// Wait for one answer, giving up after `timeout`. The registration is removed on
    /// every exit path — a timed-out question must not swallow a late answer's sender.
    pub async fn wait(&self, question_id: &str, timeout: Duration) -> Option<Answer> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut waiters = self.waiters.lock().expect("mailbox mutex poisoned");
            waiters.insert(question_id.to_string(), tx);
        }
        let delivered = tokio::time::timeout(timeout, rx).await;
        self.waiters
            .lock()
            .expect("mailbox mutex poisoned")
            .remove(question_id);
        match delivered {
            Ok(Ok(answer)) => Some(answer),
            _ => None,
        }
    }

    /// Hand an answer to its waiter, if one is still parked. `false` means nobody is
    /// listening (already timed out, or the worker restarted): the caller has usually
    /// persisted the answer anyway.
    pub fn deliver(&self, answer: &Answer) -> bool {
        let sender = self
            .waiters
            .lock()
            .expect("mailbox mutex poisoned")
            .remove(&answer.question_id);
        match sender {
            Some(sender) => sender.send(answer.clone()).is_ok(),
            None => false,
        }
    }
}

/// What one delegated execution needs to build its extension: who asked and which
/// session to blame. The backend template is shared across concurrent tasks; this is
/// the per-task part.
#[derive(Debug, Clone)]
pub struct TaskDelegatorCtx {
    pub task_id: TaskId,
    pub session_id: String,
}

/// The `ask_delegator` capability attached to delegated runs. One instance per task —
/// the task/session context is fixed at construction so the model cannot spoof it
/// through tool arguments.
pub struct AskDelegator {
    store: Arc<crate::queue::TaskStore>,
    mailbox: Arc<AnswerMailbox>,
    timeout_secs: u64,
    max_open_questions: u32,
    task_ctx: TaskDelegatorCtx,
}

impl AskDelegator {
    pub fn new(
        store: Arc<crate::queue::TaskStore>,
        mailbox: Arc<AnswerMailbox>,
        timeout_secs: u64,
        max_open_questions: u32,
        task_ctx: TaskDelegatorCtx,
    ) -> Self {
        Self {
            store,
            mailbox,
            timeout_secs,
            max_open_questions,
            task_ctx,
        }
    }

    async fn handle_ask(
        &self,
        task_id: &TaskId,
        session_id: &str,
        body: String,
        options: Vec<QuestionOption>,
        default_option: Option<String>,
    ) -> Result<String, String> {
        if self
            .store
            .open_questions(task_id)
            .map_err(|e| e.to_string())?
            >= self.max_open_questions as u64
        {
            self.store
                .record_blocked_once(
                    task_id,
                    &format!(
                        "question cap ({}) reached; further questions refused",
                        self.max_open_questions
                    ),
                )
                .map_err(|e| e.to_string())?;
            return Err(format!(
                "the delegator already holds {} unanswered questions for this task; \
                 decide without asking or wrap up honestly",
                self.max_open_questions
            ));
        }

        let question = Question {
            id: ulid::Ulid::new().to_string(),
            correlation_id: String::new(),
            task_id: task_id.clone(),
            session_id: session_id.to_string(),
            body,
            options,
            default_option,
        };
        let question = self
            .store
            .record_question(task_id, question)
            .map_err(|e| e.to_string())?;

        let waited = self
            .mailbox
            .wait(&question.id, Duration::from_secs(self.timeout_secs))
            .await;
        match waited {
            Some(answer) => {
                self.store
                    .record_answer(task_id, &answer, false)
                    .map_err(|e| e.to_string())?;
                Ok(render_answer(&answer))
            }
            None => match question.default_option.clone() {
                Some(default) => {
                    let fallback = Answer {
                        question_id: question.id.clone(),
                        chosen_option: Some(default.clone()),
                        body: format!(
                            "no answer within {}s; the question's declared default was applied",
                            self.timeout_secs
                        ),
                    };
                    self.store
                        .record_answer(task_id, &fallback, true)
                        .map_err(|e| e.to_string())?;
                    Ok(format!(
                        "The delegator did not answer within {}s. Applying the question's \
                         declared default:\nchosen option: {default}",
                        self.timeout_secs
                    ))
                }
                None => {
                    self.store
                        .record_blocked_once(
                            task_id,
                            &format!("question {} timed out without a default", question.id),
                        )
                        .map_err(|e| e.to_string())?;
                    Err(format!(
                        "the delegator did not answer question {} within {}s and no default \
                         was declared; proceed with your best judgment or wrap up honestly",
                        question.id, self.timeout_secs
                    ))
                }
            },
        }
    }
}

/// The tool result the model reads. Options carry consequences precisely so this text
/// can quote them back with the choice.
fn render_answer(answer: &Answer) -> String {
    let choice = match &answer.chosen_option {
        Some(option) => format!("chosen option: {option}"),
        None => "the delegator replied without choosing an option".to_string(),
    };
    format!(
        "The delegator answered question {}:\n{choice}\nmessage: {}\n\nContinue the task with this answer.",
        answer.question_id, answer.body
    )
}

#[async_trait]
impl RuntimeExtension for AskDelegator {
    fn tools(&self) -> Vec<ToolDef> {
        vec![ToolDef::new(
            ASK_DELEGATOR,
            "Ask the delegating agent a blocking question about the task. Use it when you \
             cannot proceed sensibly without a decision only the delegator can make — never \
             for information you can discover yourself. Propose concrete options with their \
             consequences so the delegator can choose instead of researching.",
            json!({
                "type": "object",
                "properties": {
                    "body": {"type": "string",
                        "description": "What is blocking and what was already tried."},
                    "options": {"type": "array", "minItems": 1, "items": {
                        "type": "object",
                        "properties": {
                            "label": {"type": "string"},
                            "consequence": {"type": "string",
                                "description": "What choosing this option means."}
                        },
                        "required": ["label", "consequence"]
                    }},
                    "default_option": {"type": "string",
                        "description": "Label of the option to apply if nobody answers in time."}
                },
                "required": ["body", "options"]
            }),
        )]
    }

    async fn invoke(&self, call: &ToolInvocation) -> Option<Result<String, String>> {
        if call.name != ASK_DELEGATOR {
            return None;
        }
        // A claimed call with malformed arguments must still answer in-band, so the
        // model can correct it — falling through would surface an "unknown tool" error.
        let Some((body, options, default_option)) = parse_question_args(&call.arguments) else {
            return Some(Err(
                "ask_delegator needs a non-empty 'body' and at least one option \
                 with 'label' and 'consequence'"
                    .into(),
            ));
        };
        Some(
            self.handle_ask(
                &self.task_ctx.task_id.clone(),
                &self.task_ctx.session_id.clone(),
                body,
                options,
                default_option,
            )
            .await,
        )
    }
}

/// Pull `(body, options, default_option)` out of the model's arguments. `None` means
/// malformed — handed back to the model as a plain tool error.
fn parse_question_args(
    arguments: &serde_json::Value,
) -> Option<(String, Vec<QuestionOption>, Option<String>)> {
    let body = arguments.get("body")?.as_str()?.to_string();
    let raw_options = arguments.get("options")?.as_array()?;
    let mut options = Vec::new();
    for option in raw_options {
        options.push(QuestionOption {
            label: option.get("label")?.as_str()?.to_string(),
            consequence: option.get("consequence")?.as_str()?.to_string(),
        });
    }
    if options.is_empty() || body.trim().is_empty() {
        return None;
    }
    let default_option = arguments
        .get("default_option")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    Some((body, options, default_option))
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_delegate_contract::TaskSpec;

    struct Fixture {
        _tmp: tempfile::TempDir,
        store: Arc<crate::queue::TaskStore>,
        mailbox: Arc<AnswerMailbox>,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::queue::TaskStore::open(tmp.path()).unwrap());
        let mailbox = Arc::new(AnswerMailbox::default());
        Fixture {
            _tmp: tmp,
            store,
            mailbox,
        }
    }

    fn spec() -> TaskSpec {
        use liberado_delegate_contract::{Acceptance, TaskBudget, TaskGrant};
        TaskSpec {
            id: TaskId("01ASKTASK0000000000000TEST".into()),
            project: "p".into(),
            repository: "o/r".into(),
            base_branch: "main".into(),
            goal: "g".into(),
            success_criteria: vec![],
            acceptance: Acceptance::default(),
            budget: TaskBudget::default(),
            grant: TaskGrant::default(),
        }
    }

    fn ext(f: &Fixture) -> AskDelegator {
        let _ = f.store.submit(&spec()).unwrap();
        AskDelegator::new(
            f.store.clone(),
            f.mailbox.clone(),
            1,
            2,
            TaskDelegatorCtx {
                task_id: TaskId("01ASKTASK0000000000000TEST".into()),
                session_id: "sess".into(),
            },
        )
    }

    fn ask_args(body: &str, default: Option<&str>) -> serde_json::Value {
        let mut args = json!({
            "body": body,
            "options": [
                {"label": "left", "consequence": "fast"},
                {"label": "right", "consequence": "slow"}
            ]
        });
        if let Some(default) = default {
            args["default_option"] = json!(default);
        }
        args
    }

    #[tokio::test]
    async fn only_its_own_tool_is_claimed_and_malformed_args_are_errors() {
        let f = fixture();
        let ext = ext(&f);
        assert!(
            ext.invoke(&ToolInvocation::new("1", "read_file", json!({})))
                .await
                .is_none()
        );
        let malformed = ext
            .invoke(&ToolInvocation::new(
                "2",
                ASK_DELEGATOR,
                json!({"body": ""}),
            ))
            .await
            .unwrap();
        assert!(malformed.is_err(), "empty options must be refused");
    }

    /// The park: invoke blocks until `deliver`, then the answer comes back as the
    /// tool result. This is the round-trip D2 acceptance asks for.
    #[tokio::test]
    async fn ask_parks_until_the_answer_arrives_then_returns_it_in_band() {
        let f = fixture();
        let waiter = Arc::new(ext(&f));
        let parked = tokio::spawn(async move {
            let call = ToolInvocation::new("1", ASK_DELEGATOR, ask_args("which way?", None));
            waiter.invoke(&call).await
        });

        // Let the ask register before answering.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let answer = Answer {
            question_id: f
                .store
                .replay("01ASKTASK0000000000000TEST")
                .unwrap()
                .iter()
                .find(|e| e.kind == liberado_delegate_contract::EventKind::Question)
                .map(|e| e.payload["question"]["id"].as_str().unwrap().to_string())
                .expect("question on the stream"),
            chosen_option: Some("right".into()),
            body: "take the slow road".into(),
        };
        assert!(
            f.mailbox.deliver(&answer),
            "a parked waiter must receive it"
        );

        let result = parked.await.unwrap().unwrap().unwrap();
        assert!(result.contains("chosen option: right"), "{result}");
        assert!(result.contains("take the slow road"), "{result}");
    }

    #[tokio::test]
    async fn timeout_applies_the_declared_default_and_records_it_as_auto() {
        let f = fixture();
        let ext = ext(&f);
        let result = ext
            .invoke(&ToolInvocation::new(
                "1",
                ASK_DELEGATOR,
                ask_args("which way?", Some("left")),
            ))
            .await
            .unwrap()
            .unwrap();
        assert!(result.contains("chosen option: left"), "{result}");

        let record = f
            .store
            .open_questions(&TaskId("01ASKTASK0000000000000TEST".into()))
            .unwrap();
        assert_eq!(record, 0, "the auto-answer settles the question");
        let answered = f
            .store
            .replay("01ASKTASK0000000000000TEST")
            .unwrap()
            .into_iter()
            .find(|e| e.payload.get("answered").is_some())
            .expect("auto-answer event");
        assert_eq!(
            answered.payload["answered"]["timed_out_default"],
            json!(true)
        );
    }

    #[tokio::test]
    async fn timeout_without_a_default_refuses_and_marks_blocked_once() {
        let f = fixture();
        let ext = ext(&f);
        let first = ext
            .invoke(&ToolInvocation::new(
                "1",
                ASK_DELEGATOR,
                ask_args("which way?", None),
            ))
            .await
            .unwrap()
            .unwrap_err();
        assert!(first.contains("no default was declared"), "{first}");
        // A second timed-out ask must not add another Blocked marker.
        let _second = ext
            .invoke(&ToolInvocation::new(
                "2",
                ASK_DELEGATOR,
                ask_args("again?", None),
            ))
            .await
            .unwrap()
            .unwrap_err();
        let blocked = f
            .store
            .replay("01ASKTASK0000000000000TEST")
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == liberado_delegate_contract::EventKind::Blocked)
            .count();
        assert_eq!(blocked, 1);
    }

    #[tokio::test]
    async fn past_the_cap_further_questions_are_refused_without_asking() {
        // Cap is 2; two asks with no default time out (1s each) and stay open.
        let f = fixture();
        let ext = ext(&f);
        for n in 0..2 {
            let refused_or_timeout = ext
                .invoke(&ToolInvocation::new(
                    format!("{n}"),
                    ASK_DELEGATOR,
                    ask_args("q?", None),
                ))
                .await
                .unwrap();
            assert!(refused_or_timeout.is_err(), "no default: must time out");
        }
        let refused = ext
            .invoke(&ToolInvocation::new(
                "9",
                ASK_DELEGATOR,
                ask_args("one more?", None),
            ))
            .await
            .unwrap()
            .unwrap_err();
        assert!(refused.contains("unanswered questions"), "{refused}");
        // The refusal itself asked nothing: still exactly two open questions.
        assert_eq!(
            f.store
                .open_questions(&TaskId("01ASKTASK0000000000000TEST".into()))
                .unwrap(),
            2
        );
    }
}
