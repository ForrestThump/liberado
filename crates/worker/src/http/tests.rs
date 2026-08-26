//! The SSE endpoint's two connection shapes: a client arriving after completion gets
//! the journal replay and a clean close; a client attached mid-run sees live events
//! through to the terminal one. Driven at the handler level with the real store.

use std::sync::Arc;

use super::{State, task_events};
use crate::config::WorkerSettings;
use crate::http::AppState;
use crate::queue::TaskStore;
use crate::runner::RunContext;
use axum::extract::Path;
use axum::response::IntoResponse;
use liberado_delegate_contract::WorkerEvent;
use liberado_delegate_contract::{Acceptance, Answer, TaskBudget, TaskGrant, TaskId, TaskSpec};

fn spec(id: &str) -> TaskSpec {
    TaskSpec {
        id: TaskId(id.to_string()),
        project: "demo".into(),
        repository: "local/repo".into(),
        base_branch: "main".into(),
        goal: "g".into(),
        success_criteria: vec![],
        acceptance: Acceptance::default(),
        budget: TaskBudget::default(),
        grant: TaskGrant::default(),
    }
}

struct Harness {
    #[allow(dead_code)]
    tmp: tempfile::TempDir,
    state: Arc<AppState>,
    store: Arc<TaskStore>,
}

impl Harness {
    /// Drive the answers handler directly (no TCP): the endpoint is thin over the
    /// store + mailbox, and the mailbox round-trip has its own tests in `ask`.
    async fn answer(&self, task_id: &str, answer: &Answer) -> axum::http::StatusCode {
        use super::post_answer;
        let response = post_answer(
            State(Arc::clone(&self.state)),
            Path(task_id.to_string()),
            axum::Json(answer.clone()),
        )
        .await;
        response.status()
    }
}

fn harness() -> Harness {
    let tmp = tempfile::tempdir().expect("tempdir");
    let settings = Arc::new(WorkerSettings {
        bind: "127.0.0.1:0".into(),
        token: "t".into(),
        data_dir: tmp.path().join("data"),
        config_dir: None,
        model: None,
        forge_url: None,
        forge_token: String::new(),
        forge_insecure_tls: false,
        clone_base_url: None,
        max_concurrent: 1,
        question_timeout_secs: 1,
        max_open_questions: 3,
    });
    let store = Arc::new(TaskStore::open(&settings.data_dir).expect("store"));
    // The runner is never invoked by these tests; events flow through the store.
    let run = RunContext {
        settings: settings.clone(),
        store: store.clone(),
        backends: Arc::new(crate::runner::FixedBackend(Arc::new(NoopBackend))),
        forge: None,
    };
    let state = Arc::new(AppState {
        slots: Arc::new(tokio::sync::Semaphore::new(1)),
        settings,
        mailbox: Arc::new(crate::ask::AnswerMailbox::default()),
        store: store.clone(),
        run,
    });
    Harness { tmp, state, store }
}

/// A backend that never runs; the stream tests drive transitions directly.
struct NoopBackend;

#[async_trait::async_trait]
impl liberado_coder_core::CoderBackend for NoopBackend {
    fn name(&self) -> &str {
        "noop"
    }
    async fn run(
        &self,
        _request: liberado_coder_core::CoderRunRequest,
    ) -> Result<liberado_coder_core::CoderRunResult, liberado_coder_core::CoderError> {
        Err(liberado_coder_core::CoderError::Backend(
            "not used in stream tests".into(),
        ))
    }
}

/// Drive the handler's response body to completion and decode the SSE frames. The
/// server closes the stream on the terminal event; the deadline only guards against a
/// regression that would park a client forever.
async fn collect_frames(state: Arc<AppState>, task_id: &str) -> Vec<(String, String)> {
    use futures::StreamExt;
    let response = task_events(State(state), Path(task_id.to_string()))
        .await
        .into_response();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let mut stream = response.into_body().into_data_stream();
    let mut decoder = chat_client_contract::native::SseDecoder::default();
    let mut frames: Vec<(String, String)> = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    // Termination is the SERVER's job: it must close the body after the terminal
    // event. A local name-based break here would mask a server that keeps the
    // connection parked forever — which is precisely the regression worth catching.
    loop {
        match tokio::time::timeout_at(deadline, stream.next()).await {
            Err(_) => panic!("stream hung past 10s; server failed to terminate"),
            Ok(None) => break,
            Ok(Some(chunk)) => {
                let chunk = chunk.expect("body read");
                for event in decoder.push(std::str::from_utf8(&chunk).expect("utf8")) {
                    frames.push((event.event, event.data));
                }
            }
        }
    }
    frames
}

#[tokio::test]
async fn a_connection_after_completion_replays_and_closes() {
    let h = harness();
    h.store
        .submit(&spec("01JSSE0000000000DONE"))
        .expect("submit");
    h.store
        .mark_running(&TaskId("01JSSE0000000000DONE".into()), "s")
        .expect("running");
    h.store
        .finish(
            &TaskId("01JSSE0000000000DONE".into()),
            liberado_delegate_contract::TaskStatus::PrOpened {
                url: "http://forge/pr/1".into(),
            },
        )
        .expect("finish");

    let frames = collect_frames(h.state.clone(), "01JSSE0000000000DONE").await;
    let names: Vec<&str> = frames.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        vec!["status_changed", "status_changed", "pr_ready"],
        "{frames:?}"
    );
}

#[tokio::test]
async fn a_midrun_client_receives_replay_then_live_through_terminal() {
    let h = harness();
    let id = "01JSSE0000000000LIVE";
    h.store.submit(&spec(id)).expect("submit");

    // Attach while queued, then advance the task from another handle.
    let state = h.state.clone();
    let mover = tokio::spawn(async move {
        let tid = TaskId(id.to_string());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        state.store.mark_running(&tid, "s").expect("running");
        state
            .store
            .finish(
                &tid,
                liberado_delegate_contract::TaskStatus::PrOpened {
                    url: "http://forge/pr/9".into(),
                },
            )
            .expect("finish");
    });

    let frames = collect_frames(h.state.clone(), id).await;
    mover.await.expect("mover");
    let names: Vec<&str> = frames.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        vec!["status_changed", "status_changed", "pr_ready"],
        "{frames:?}"
    );

    // Every frame carries its correlation id inside the data for client-side dedupe.
    for (_, data) in &frames {
        let event: WorkerEvent = serde_json::from_str(data).expect("data is a WorkerEvent");
        let expected_prefix = format!("delegate:{id}:");
        assert!(
            event.correlation_id.starts_with(&expected_prefix),
            "{event:?}"
        );
    }
}

#[tokio::test]
async fn an_answer_settles_the_question_and_reports_delivery() {
    use liberado_delegate_contract::{Question, QuestionOption};
    let h = harness();
    let task = TaskId("01ANSWERTEST".into());
    let _ = h.store.submit(&spec("01ANSWERTEST")).expect("submit");
    let _ = h
        .store
        .record_question(
            &task,
            Question {
                id: "q1".into(),
                correlation_id: String::new(),
                task_id: task.clone(),
                session_id: "sess".into(),
                body: "left or right?".into(),
                options: vec![QuestionOption {
                    label: "left".into(),
                    consequence: "fast".into(),
                }],
                default_option: None,
            },
        )
        .expect("question recorded");
    // A parked ask registers its waiter on the same mailbox the handler uses.
    let parked = {
        let state = Arc::clone(&h.state);
        tokio::spawn(async move {
            crate::ask::AnswerMailbox::wait(
                state.mailbox.as_ref(),
                "q1",
                std::time::Duration::from_secs(5),
            )
            .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let status = h
        .answer(
            "01ANSWERTEST",
            &Answer {
                question_id: "q1".into(),
                chosen_option: Some("left".into()),
                body: "go left".into(),
            },
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(parked.await.expect("join").is_some(), "waiter got it");
    assert_eq!(
        h.store.open_questions(&task).expect("open"),
        0,
        "the endpoint persisted the answer"
    );
}

#[tokio::test]
async fn unknown_questions_and_unknown_tasks_are_rejected_honestly() {
    let h = harness();
    let _ = h.store.submit(&spec("01NOTASKETEST")).expect("submit");

    let no_question = h
        .answer(
            "01NOTASKETEST",
            &Answer {
                question_id: "ghost".into(),
                chosen_option: None,
                body: String::new(),
            },
        )
        .await;
    assert_eq!(no_question, axum::http::StatusCode::NOT_FOUND);

    let no_task = h
        .answer(
            "01ABSENTTASK",
            &Answer {
                question_id: "q1".into(),
                chosen_option: None,
                body: String::new(),
            },
        )
        .await;
    assert_eq!(no_task, axum::http::StatusCode::NOT_FOUND);
}
