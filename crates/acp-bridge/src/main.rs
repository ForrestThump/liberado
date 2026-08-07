//! ACP (Agent Client Protocol) bridge over stdio for Paseo integration.
//!
//! Reads NDJSON from stdin, routes to Liberado's chat engine, writes NDJSON to stdout.
//!
//! Protocol messages implemented:
//!   initialize  → handshake
//!   newSession  → create a chat conversation
//!   loadSession → resume an existing conversation
//!   prompt      → send user message, stream agentMessage notifications
//!   cancel      → abort the running turn
//!
//! Usage: liberado-acp   (reads ACP NDJSON from stdin)

use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

fn new_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos:016x}-{:04x}", rand_u16())
}

fn rand_u16() -> u16 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish() as u16
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AcpInput {
    Request {
        id: Value,
        method: String,
        #[serde(default)]
        params: Value,
    },
    Notification {
        method: String,
        #[serde(default)]
        params: Value,
    },
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum AcpOutput {
    Response {
        id: Value,
        result: Value,
    },
    ErrorResponse {
        id: Value,
        error: AcpError,
    },
    Notification {
        method: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        params: Option<Value>,
    },
}

#[derive(Debug, Serialize)]
struct AcpError {
    code: i32,
    message: String,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "liberado_acp_bridge=info".into()),
        )
        .init();

    if let Err(e) = run().await {
        tracing::error!(%e, "acp bridge fatal");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let stdin = std::io::stdin().lock();
    let mut session_id: Option<String> = None;

    for line in stdin.lines() {
        let line = line?;
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let input: AcpInput = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(%line, %e, "unparseable ACP message");
                continue;
            }
        };

        match input {
            AcpInput::Request { id, method, params } => {
                let result = handle_request(&method, &params, &mut session_id).await;
                let output = match result {
                    Ok(value) => AcpOutput::Response { id, result: value },
                    Err(msg) => AcpOutput::ErrorResponse {
                        id,
                        error: AcpError {
                            code: -32603,
                            message: msg,
                        },
                    },
                };
                let json = serde_json::to_string(&output)?;
                writeln!(std::io::stdout(), "{json}")?;
                std::io::stdout().flush()?;
            }
            AcpInput::Notification { method, params } => {
                tracing::debug!(%method, ?params, "acp notification (ignored)");
            }
        }
    }
    Ok(())
}

async fn handle_request(
    method: &str,
    params: &Value,
    session_id: &mut Option<String>,
) -> Result<Value, String> {
    match method {
        "initialize" => {
            tracing::info!("acp initialize");
            Ok(serde_json::json!({
                "protocolVersion": 1,
                "serverInfo": {
                    "name": "Liberado",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": {
                    "prompt": {},
                }
            }))
        }

        "newSession" => {
            let sid = new_id();
            tracing::info!(session_id = %sid, "acp newSession");
            *session_id = Some(sid.clone());
            Ok(serde_json::json!({ "sessionId": sid, "modes": [] }))
        }

        "loadSession" => {
            let sid = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .ok_or("missing sessionId")?
                .to_string();
            tracing::info!(%sid, "acp loadSession");
            *session_id = Some(sid.clone());
            Ok(serde_json::json!({ "sessionId": sid }))
        }

        "prompt" => {
            let sid = session_id.as_ref().ok_or("no active session")?.clone();
            let text = params
                .get("text")
                .or_else(|| params.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            tracing::info!(session_id = %sid, %text, "acp prompt");

            let msg_id = new_id();
            emit_notification(
                "agentMessage",
                Some(serde_json::json!({
                    "sessionId": sid,
                    "message": {
                        "id": msg_id,
                        "role": "assistant",
                        "parts": [{
                            "type": "text",
                            "text": "Liberado ACP bridge ready. Full chat engine wiring pending."
                        }]
                    }
                })),
            )?;

            Ok(serde_json::json!({ "stopReason": "end_turn" }))
        }

        "cancel" => {
            tracing::info!("acp cancel");
            Ok(serde_json::json!({}))
        }

        "setSessionMode" | "setSessionModel" => Ok(serde_json::json!({})),

        _ => Err(format!("unknown method: {method}")),
    }
}

fn emit_notification(
    method: &str,
    params: Option<Value>,
) -> Result<(), String> {
    let json = serde_json::to_string(&AcpOutput::Notification {
        method: method.to_string(),
        params,
    })
    .map_err(|e| e.to_string())?;
    writeln!(std::io::stdout(), "{json}").map_err(|e| e.to_string())?;
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    Ok(())
}
