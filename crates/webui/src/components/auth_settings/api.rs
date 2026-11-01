//! HTTP for passkeys and signed-in sessions on the settings page.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct PasskeyRow {
    pub id: i64,
    pub name: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthSessionRow {
    pub id: String,
    pub method: String,
    pub created_at: String,
    pub last_seen_at: String,
    pub user_agent: Option<String>,
    pub current: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct SessionProbe {
    pub method: Option<String>,
}

pub async fn fetch_passkeys(api_base: &str) -> Result<Vec<PasskeyRow>, String> {
    let url = format!("{api_base}/api/auth/passkeys");
    let resp = crate::http::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Passkeys unavailable ({})", resp.status()));
    }
    resp.json().await.map_err(|e| format!("Bad response: {e}"))
}

pub async fn fetch_sessions(api_base: &str) -> Result<Vec<AuthSessionRow>, String> {
    let url = format!("{api_base}/api/auth/sessions");
    let resp = crate::http::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Sessions unavailable ({})", resp.status()));
    }
    resp.json().await.map_err(|e| format!("Bad response: {e}"))
}

pub async fn session_method(api_base: &str) -> Result<Option<String>, String> {
    let resp = crate::http::session_status(api_base)
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("Session probe failed ({})", resp.status()));
    }
    let body: SessionProbe = resp.json().await.map_err(|e| e.to_string())?;
    Ok(body.method)
}

pub async fn delete_passkey(api_base: &str, id: i64) -> Result<(), String> {
    let url = format!("{api_base}/api/auth/passkeys/{id}");
    let resp = crate::http::delete(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if resp.status().is_success() || resp.status() == reqwest::StatusCode::NO_CONTENT {
        Ok(())
    } else {
        Err(format!("Remove passkey failed ({})", resp.status()))
    }
}

pub async fn revoke_session(api_base: &str, public_id: &str) -> Result<(), String> {
    let url = format!("{api_base}/api/auth/sessions/{public_id}");
    let resp = crate::http::delete(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if resp.status().is_success() || resp.status() == reqwest::StatusCode::NO_CONTENT {
        Ok(())
    } else {
        Err(format!("Sign out failed ({})", resp.status()))
    }
}

pub fn format_session_method(method: &str) -> &'static str {
    match method {
        "passkey" => "Passkey",
        "token" => "Emergency token",
        _ => "Session",
    }
}
