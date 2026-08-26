use std::error::Error;

use liberado_delegate_contract::{SubmitOutcome, TaskRecord, TaskSpec, WorkerHealth, routes};

/// `liberado delegate …` — the delegator-side client of a worker's control plane
/// (`docs/future-work/delegate-network-plan.md`). Thin async HTTP over the shared
/// contract, routed like `chat` rather than through the sync router: a blocking client
/// panics when its runtime drops inside the daemon-adjacent dispatch context. All logic
/// lives on the worker; this file owns argument grammar, transport, and rendering.
pub async fn run(args: &mut impl Iterator<Item = String>) -> Result<(), Box<dyn Error>> {
    match args.next().as_deref() {
        // Submit owns its grammar (file path); every other verb is routed centrally
        // so adding one costs this entry point nothing.
        Some("submit") => cmd_submit(args).await,
        Some(name) => router::dispatch(name, args).await,
        None => Err(usage("unknown or missing subcommand").into()),
    }
}

fn usage(message: &str) -> String {
    format!(
        "{message}\n\n\
         usage:\n  \
         liberado delegate submit <task.json> [--endpoint URL] [--token-env VAR]\n  \
         liberado delegate status <task-id>   [--endpoint URL] [--token-env VAR]\n  \
         liberado delegate watch <task-id>    [--endpoint URL] [--token-env VAR]\n  \
         liberado delegate cancel <task-id>   [--endpoint URL] [--token-env VAR]\n  \
         liberado delegate health             [--endpoint URL] [--token-env VAR]\n  \\
         liberado delegate kickback <task-id> --body TEXT [--comment]\n  \\
         liberado delegate merge <task-id>    [--method squash|merge|rebase]\n  \\
         liberado delegate answer <task-id> <question-id> [--option LABEL]\n  \
             \x20\x20[--body TEXT] [--endpoint URL] [--token-env VAR]\n\n\
         Env: LIBERADO_DELEGATE_ENDPOINT (required unless --endpoint),\n\
         \x20\x20\x20\x20 LIBERADO_DELEGATE_TOKEN (default token source)"
    )
}

/// Print to stdout, exiting quietly when the pipe closes. `liberado delegate … | head`
/// is normal usage; Rust ignores SIGPIPE, so an unchecked write would panic the CLI
/// instead of doing what every other Unix tool does — stop.
pub(super) fn emit(text: &str) {
    use std::io::Write;
    let mut out = std::io::stdout();
    if out.write_all(text.as_bytes()).is_err() || out.write_all(b"\n").is_err() {
        std::process::exit(0);
    }
}

#[derive(Debug, Default, PartialEq)]
pub(super) struct Flags {
    endpoint: Option<String>,
    token_env: Option<String>,
}

fn parse_flags(
    mut args: impl Iterator<Item = String>,
    positional_name: &str,
) -> Result<(Option<String>, Flags), String> {
    let mut positional = None;
    let mut flags = Flags::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--endpoint" => flags.endpoint = Some(args.next().ok_or("--endpoint needs a value")?),
            "--token-env" => {
                flags.token_env = Some(args.next().ok_or("--token-env needs a value")?)
            }
            other if other.starts_with('-') => return Err(format!("unknown flag: {other}")),
            other => {
                if positional.replace(other.to_string()).is_some() {
                    return Err(format!("expected exactly one {positional_name}"));
                }
            }
        }
    }
    Ok((positional, flags))
}

pub(super) struct Connection {
    endpoint: String,
    token: String,
}

pub(super) fn connection(flags: &Flags) -> Result<Connection, String> {
    let endpoint = flags
        .endpoint
        .clone()
        .or_else(|| std::env::var("LIBERADO_DELEGATE_ENDPOINT").ok())
        .ok_or_else(|| {
            "no endpoint: pass --endpoint or set LIBERADO_DELEGATE_ENDPOINT".to_string()
        })?;
    let var = flags
        .token_env
        .clone()
        .unwrap_or_else(|| "LIBERADO_DELEGATE_TOKEN".to_string());
    let token = std::env::var(&var).map_err(|_| format!("{var} is not set"))?;
    Ok(Connection {
        endpoint: endpoint.trim_end_matches('/').to_string(),
        token,
    })
}

/// One authenticated request builder against the worker's control plane.
pub(super) fn request(
    connection: &Connection,
    method: reqwest::Method,
    path: &str,
) -> reqwest::RequestBuilder {
    let url = format!("{}{path}", connection.endpoint);
    reqwest::Client::new()
        .request(method, url)
        .header("Authorization", format!("Bearer {}", connection.token))
}

pub(super) async fn checked(response: reqwest::Response) -> Result<String, String> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if status.is_success() {
        Ok(body)
    } else {
        Err(format!("worker returned {status}: {body}"))
    }
}

fn pretty<T: serde::Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string_pretty(value).map_err(|error| error.to_string())
}

// --- subcommand wrappers: grammar + env + output -------------------------

async fn cmd_submit(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn Error>> {
    let (file, flags) = parse_flags(&mut args, "task.json path").map_err(|error| usage(&error))?;
    let file = file.ok_or_else(|| usage("submit needs a task.json path"))?;
    let connection = connection(&flags)?;
    let text = submit_from_file(&connection, &file).await?;
    emit(&text);
    Ok(())
}

pub(super) async fn cmd_status(
    mut args: impl Iterator<Item = String>,
) -> Result<(), Box<dyn Error>> {
    let (id, flags) = parse_flags(&mut args, "task-id").map_err(|error| usage(&error))?;
    let id = id.ok_or_else(|| usage("status needs a task-id"))?;
    let connection = connection(&flags)?;
    let record = fetch_task(&connection, &id).await?;
    emit(&pretty(&record)?);
    Ok(())
}

pub(super) async fn cmd_cancel(
    mut args: impl Iterator<Item = String>,
) -> Result<(), Box<dyn Error>> {
    let (id, flags) = parse_flags(&mut args, "task-id").map_err(|error| usage(&error))?;
    let id = id.ok_or_else(|| usage("cancel needs a task-id"))?;
    let connection = connection(&flags)?;
    let body = checked(
        request(
            &connection,
            reqwest::Method::POST,
            &routes::task_cancel(&id),
        )
        .send()
        .await
        .map_err(|error| format!("post cancel: {error}"))?,
    )
    .await?;
    let record: TaskRecord = serde_json::from_str(&body).map_err(|error| error.to_string())?;
    emit(&pretty(&record)?);
    Ok(())
}

pub(super) async fn cmd_health(
    mut args: impl Iterator<Item = String>,
) -> Result<(), Box<dyn Error>> {
    let (_none, flags) = parse_flags(&mut args, "").map_err(|error| usage(&error))?;
    let connection = connection(&flags)?;
    let health = fetch_health(&connection).await?;
    emit(&format!(
        "worker {} version {} fingerprint {}",
        health.status, health.version, health.fingerprint
    ));
    Ok(())
}

// --- injectable cores: connection in, result or reason out ---------------

/// Read a TaskSpec from disk, submit it, and render the outcome text. Duplicate
/// delivery is reported as the no-op it was, with the stored record for comparison.
async fn submit_from_file(connection: &Connection, path: &str) -> Result<String, String> {
    let raw = std::fs::read_to_string(path).map_err(|error| format!("read {path}: {error}"))?;
    let spec: TaskSpec =
        serde_json::from_str(&raw).map_err(|error| format!("{path} is not a TaskSpec: {error}"))?;
    let response = request(connection, reqwest::Method::POST, routes::TASKS)
        .json(&spec)
        .send()
        .await
        .map_err(|error| format!("post worker tasks endpoint: {error}"))?;
    let outcome: SubmitOutcome =
        serde_json::from_str(&checked(response).await?).map_err(|error| error.to_string())?;
    let header = if outcome.duplicate {
        format!(
            "duplicate submit ignored (id {} already exists); current status below",
            spec.id
        )
    } else {
        format!("submitted task {}", spec.id)
    };
    Ok(format!("{header}\n{}", pretty(&outcome.record)?))
}

async fn fetch_task(connection: &Connection, id: &str) -> Result<TaskRecord, String> {
    let body = checked(
        request(connection, reqwest::Method::GET, &routes::task(id))
            .send()
            .await
            .map_err(|error| format!("get task: {error}"))?,
    )
    .await?;
    serde_json::from_str(&body).map_err(|error| error.to_string())
}

async fn fetch_health(connection: &Connection) -> Result<WorkerHealth, String> {
    let body = checked(
        request(connection, reqwest::Method::GET, routes::HEALTH)
            .send()
            .await
            .map_err(|error| format!("get health: {error}"))?,
    )
    .await?;
    serde_json::from_str(&body).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests;

#[path = "delegate_cmd_router.rs"]
mod router;

#[path = "delegate_cmd_answer.rs"]
mod answer_cmd;

#[path = "delegate_cmd_review.rs"]
mod review_cmd;

#[path = "delegate_cmd_watch.rs"]
mod watch_cmd;
