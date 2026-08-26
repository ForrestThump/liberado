//! Question and kickback-instruction storage for [`TaskStore`] (plan §8/§10).
//!
//! A child module of `queue` on purpose: these are inherent methods on the same type,
//! and as a child they can reach the store's private paths without widening anything.
//! The wire vocabulary lives in the contract; this file owns only what lands on disk
//! and what the journal announces.

use liberado_delegate_contract::{Answer, EventKind, Question, TaskId, WorkerEvent};

use super::{QueueError, TaskStore};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QuestionRecord {
    pub question: Question,
    #[serde(default)]
    pub answer: Option<Answer>,
    /// True when the answer was chosen by the worker's timeout fallback rather than
    /// spoken by the delegator.
    #[serde(default)]
    pub timed_out_default: bool,
}

impl TaskStore {
    /// Persist a question and put it on the stream. The question file carries the
    /// event's correlation id, so an inbox item, the journal line, and the stored
    /// record all key on the same value.
    pub fn record_question(
        &self,
        task_id: &TaskId,
        mut question: Question,
    ) -> Result<Question, QueueError> {
        let event = self.mint_event(task_id, EventKind::Question, serde_json::Value::Null)?;
        question.correlation_id = event.correlation_id.clone();
        let payload = serde_json::json!({ "question": question });
        self.journal_event(WorkerEvent { payload, ..event })?;
        let dir = self.task_dir(task_id);
        std::fs::create_dir_all(dir.join("questions"))?;
        self.write_json(
            &dir.join("questions").join(format!("{}.json", question.id)),
            &QuestionRecord {
                question: question.clone(),
                answer: None,
                timed_out_default: false,
            },
        )?;
        Ok(question)
    }

    /// Store the answer to a question. `timed_out_default` marks answers the worker
    /// chose itself from the question's declared default — the delegator never spoke.
    pub fn record_answer(
        &self,
        task_id: &TaskId,
        answer: &Answer,
        timed_out_default: bool,
    ) -> Result<QuestionRecord, QueueError> {
        let path = self
            .task_dir(task_id)
            .join("questions")
            .join(format!("{}.json", answer.question_id));
        if !path.exists() {
            return Err(QueueError::QuestionNotFound(answer.question_id.clone()));
        }
        let mut stored: QuestionRecord = self.read_json(&path)?;
        stored.answer = Some(answer.clone());
        stored.timed_out_default = timed_out_default;
        self.write_json(&path, &stored)?;
        self.record_event(
            task_id,
            EventKind::StatusChanged,
            serde_json::json!({
                "answered": {
                    "question_id": answer.question_id,
                    "chosen_option": answer.chosen_option,
                    "timed_out_default": timed_out_default,
                }
            }),
        )?;
        Ok(stored)
    }

    /// Append one kickback instruction (plan §10): the durable half of a review
    /// verdict. Rounds are 1-based and derived from the journal, so restarts cannot
    /// reset the cap.
    pub fn record_instruction(&self, task_id: &TaskId, body: &str) -> Result<u32, QueueError> {
        let round = self.kickback_count(task_id)? + 1;
        let entry = serde_json::json!({ "round": round, "body": body, "at": super::now_rfc3339() });
        let dir = self.task_dir(task_id);
        std::fs::create_dir_all(&dir)?;
        use std::io::Write as _;
        let mut journal = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("instructions.jsonl"))?;
        writeln!(journal, "{entry}")?;
        Ok(round)
    }

    /// How many kickbacks this task has already absorbed.
    pub fn kickback_count(&self, id: &TaskId) -> Result<u32, QueueError> {
        let path = self.task_dir(id).join("instructions.jsonl");
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(_) => return Ok(0),
        };
        let mut count = 0u32;
        for line in raw.lines() {
            if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line)
                && entry.get("round").is_some()
            {
                count += 1;
            }
        }
        Ok(count)
    }

    /// The instruction text of one round, for seeding the re-run's goal.
    pub fn instruction_body(&self, id: &TaskId, round: u32) -> Result<Option<String>, QueueError> {
        let path = self.task_dir(id).join("instructions.jsonl");
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(_) => return Ok(None),
        };
        for line in raw.lines() {
            if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line)
                && entry.get("round").and_then(|r| r.as_u64()) == Some(round as u64)
            {
                return Ok(entry
                    .get("body")
                    .and_then(|b| b.as_str())
                    .map(str::to_string));
            }
        }
        Ok(None)
    }

    /// Questions still awaiting an answer.
    pub fn open_questions(&self, id: &TaskId) -> Result<u64, QueueError> {
        let dir = self.task_dir(id).join("questions");
        let mut open = 0;
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => return Ok(0),
        };
        for entry in entries {
            let record: QuestionRecord = self.read_json(&entry?.path())?;
            if record.answer.is_none() {
                open += 1;
            }
        }
        Ok(open)
    }

    /// Put a `Blocked` marker on the stream, once per task no matter how many times
    /// called: the delegator's inbox reports one blocked task, not twenty.
    pub fn record_blocked_once(&self, id: &TaskId, reason: &str) -> Result<(), QueueError> {
        let already = self
            .replay(&id.0)?
            .iter()
            .any(|event| event.kind == EventKind::Blocked);
        if already {
            return Ok(());
        }
        self.record_event(
            id,
            EventKind::Blocked,
            serde_json::json!({ "reason": reason }),
        )
    }
}
