//! Attach `LIBERADO_API_TOKEN` to outbound daemon requests.

use reqwest::header::AUTHORIZATION;

pub fn with_bearer(builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    match chat_client_contract::api_auth::bearer_token_from_env() {
        Some(token) => builder.header(AUTHORIZATION, format!("Bearer {token}")),
        None => builder,
    }
}
