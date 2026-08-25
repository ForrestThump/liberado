use std::error::Error;

use liberado_delegate_contract::{SubmitOutcome, TaskRecord, TaskSpec, WorkerHealth, routes};

/// `liberado delegate …` — the delegator-side client of a worker's control plane
/// (`docs/future-work/delegate-network-plan.md`). Thin async HTTP over the shared
/// contract, routed like `chat` rather than through the sync router: a blocking client
/// panics when its runtime drops inside the daemon-adjacent dispatch context. All
/// logic lives on the worker.
pub async fn run(args: &mut impl Iterator<Item = String>) -> Result<(), Box<dyn Error>> {
    match args.next().as_deref() {
        Some("submit") => submit(args).await,
        Some("status") => status(args).await,
        Some("cancel") => cancel(args).await,
        Some("health") => health(args).await,
        _ => Err(usage("unknown or missing subcommand").into()),
    }
}

fn usage(message: &str) -> String {
    format!(
        "{message}\n\n\
         usage:\n  \
         liberado delegate submit <task.json> [--endpoint URL] [--token-env VAR]\n  \
         liberado delegate status <task-id>   [--endpoint URL] [--token-env VAR]\n  \
         liberado delegate cancel <task-id>   [--endpoint URL] [--token-env VAR]\n  \
         liberado delegate health             [--endpoint URL] [--token-env VAR]\n\n\
         Env: LIBERADO_DELEGATE_ENDPOINT (required unless --endpoint),\n\
         \x20\x20\x20\x20 LIBERADO_DELEGATE_TOKEN (default token source)"
    )
}

#[derive(Debug, Default, PartialEq)]
struct Flags {
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

struct Connection {
    endpoint: String,
    token: String,
}

fn connection(flags: &Flags) -> Result<Connection, String> {
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
fn request(
    connection: &Connection,
    method: reqwest::Method,
    path: &str,
) -> reqwest::RequestBuilder {
    let url = format!("{}{path}", connection.endpoint);
    reqwest::Client::new()
        .request(method, url)
        .header("Authorization", format!("Bearer {}", connection.token))
}

async fn checked(response: reqwest::Response) -> Result<String, String> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if status.is_success() {
        Ok(body)
    } else {
        Err(format!("worker returned {status}: {body}"))
    }
}

async fn submit(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn Error>> {
    // First positional is the subcommand's file argument; re-parse what follows it.
    let (file, flags) = parse_flags(&mut args, "task.json path").map_err(|error| usage(&error))?;
    let file = file.ok_or_else(|| usage("submit needs a task.json path"))?;
    let connection = connection(&flags)?;

    let raw = std::fs::read_to_string(&file).map_err(|error| format!("read {file}: {error}"))?;
    let spec: TaskSpec =
        serde_json::from_str(&raw).map_err(|error| format!("{file} is not a TaskSpec: {error}"))?;

    let body = checked(
        request(&connection, reqwest::Method::POST, routes::TASKS)
            .json(&spec)
            .send()
            .await
            .map_err(|error| format!("post worker tasks endpoint: {error}"))?,
    )
    .await?;
    let outcome: SubmitOutcome = serde_json::from_str(&body)?;

    if outcome.duplicate {
        println!(
            "duplicate submit ignored (id {} already exists); current status below",
            spec.id
        );
    } else {
        println!("submitted task {}", spec.id);
    }
    println!("{}", serde_json::to_string_pretty(&outcome.record)?);
    Ok(())
}

async fn status(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn Error>> {
    let (id, flags) = parse_flags(&mut args, "task-id").map_err(|error| usage(&error))?;
    let id = id.ok_or_else(|| usage("status needs a task-id"))?;
    let record = fetch_task(&id, &flags).await?;
    println!("{}", serde_json::to_string_pretty(&record)?);
    Ok(())
}

async fn cancel(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn Error>> {
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
    let record: TaskRecord = serde_json::from_str(&body)?;
    println!("{}", serde_json::to_string_pretty(&record)?);
    Ok(())
}

async fn health(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn Error>> {
    let (_none, flags) = parse_flags(&mut args, "").map_err(|error| usage(&error))?;
    let connection = connection(&flags)?;
    let body = checked(
        request(&connection, reqwest::Method::GET, routes::HEALTH)
            .send()
            .await
            .map_err(|error| format!("get health: {error}"))?,
    )
    .await?;
    let health: WorkerHealth = serde_json::from_str(&body)?;
    println!(
        "worker {} version {} fingerprint {}",
        health.status, health.version, health.fingerprint
    );
    Ok(())
}

async fn fetch_task(id: &str, flags: &Flags) -> Result<TaskRecord, String> {
    let connection = connection(flags)?;
    let body = checked(
        request(&connection, reqwest::Method::GET, &routes::task(id))
            .send()
            .await
            .map_err(|error| format!("get task: {error}"))?,
    )
    .await?;
    serde_json::from_str(&body).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests;
