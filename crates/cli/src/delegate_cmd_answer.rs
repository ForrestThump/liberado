//! `liberado delegate answer` — reply to a parked question (plan §8). Split from the
//! parent router for module health, same as the watch subcommand.

use std::error::Error;

use liberado_delegate_contract::{Answer, AnswerAck, AnswerKind};

use super::{Connection, Flags, checked, connection, emit, request, routes};
/// `liberado delegate answer <task-id> <question-id> [--option LABEL] [--body TEXT]`
/// — reply to a parked question. An empty body is allowed only with an option; the
/// worker decides delivery by whether a run is still parked on the question.
pub(super) async fn run(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn Error>> {
    let (positional, flags) = parse_answer_args(&mut args).map_err(|error| super::usage(&error))?;
    let [task_id, question_id] = positional.as_slice() else {
        return Err(super::usage("answer needs <task-id> and <question-id>").into());
    };
    let connection = connection(&to_flags(&flags))?;
    let ack = post_answer(
        &connection,
        &Answer {
            question_id: question_id.clone(),
            kind: AnswerKind::Question,
            chosen_option: flags.option,
            body: flags.body.unwrap_or_default(),
        },
        task_id,
    )
    .await?;
    emit(if ack.delivered {
        "answer delivered to the parked run"
    } else {
        "answer recorded, but no parked run is waiting (it timed out or the worker restarted)"
    });
    Ok(())
}

#[derive(Debug, Default)]
pub(super) struct AnswerFlags {
    endpoint: Option<String>,
    token_env: Option<String>,
    option: Option<String>,
    body: Option<String>,
}

pub(super) fn to_flags(answer: &AnswerFlags) -> Flags {
    Flags {
        endpoint: answer.endpoint.clone(),
        token_env: answer.token_env.clone(),
    }
}

/// Two positionals plus answer-specific flags; separate from [`parse_flags`] because
/// the generic one admits exactly one positional.
fn parse_answer_args(
    mut args: impl Iterator<Item = String>,
) -> Result<(Vec<String>, AnswerFlags), String> {
    let mut positionals = Vec::new();
    let mut flags = AnswerFlags::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--endpoint" => flags.endpoint = Some(args.next().ok_or("--endpoint needs a value")?),
            "--token-env" => {
                flags.token_env = Some(args.next().ok_or("--token-env needs a value")?)
            }
            "--option" => flags.option = Some(args.next().ok_or("--option needs a value")?),
            "--body" => flags.body = Some(args.next().ok_or("--body needs a value")?),
            other if other.starts_with('-') => return Err(format!("unknown flag: {other}")),
            other => positionals.push(other.to_string()),
        }
    }
    if positionals.len() > 2 {
        return Err("answer takes exactly two positionals: <task-id> <question-id>".into());
    }
    Ok((positionals, flags))
}

async fn post_answer(
    connection: &Connection,
    answer: &Answer,
    task_id: &str,
) -> Result<AnswerAck, String> {
    let response = request(
        connection,
        reqwest::Method::POST,
        &routes::task_answers(task_id),
    )
    .json(answer)
    .send()
    .await
    .map_err(|error| format!("post answer: {error}"))?;
    serde_json::from_str(&checked(response).await?).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::super::Flags;
    use super::{parse_answer_args, to_flags};

    #[test]
    fn answer_arg_grammar_separates_positionals_from_flags() {
        let (positionals, flags) = parse_answer_args(
            ["01T", "01Q", "--option", "left", "--body", "do it"]
                .iter()
                .map(|s| s.to_string()),
        )
        .expect("parse");
        assert_eq!(positionals, vec!["01T".to_string(), "01Q".to_string()]);
        assert_eq!(flags.option.as_deref(), Some("left"));
        assert_eq!(flags.body.as_deref(), Some("do it"));
        assert_eq!(
            to_flags(&flags),
            Flags {
                endpoint: None,
                token_env: None
            }
        );

        let too_many = parse_answer_args(["a", "b", "c"].iter().map(|s| s.to_string()));
        assert!(too_many.is_err());
    }
}

#[cfg(test)]
#[path = "delegate_cmd_answer_tests.rs"]
mod mock_tests;
