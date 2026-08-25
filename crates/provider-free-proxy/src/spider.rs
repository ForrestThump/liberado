//! Client for spider-mcp's Firecrawl-compatible REST surface — the scrape fallback.
//!
//! When OpenRouter's Benchmarks API cannot answer, ranking falls back to scraping public
//! leaderboard pages. The scraping itself is delegated to
//! [liberado-spider-mcp](https://github.com/ForrestThump/liberado-spider-mcp) (`POST /v1/scrape`)
//! rather than done inline here: it already owns the polite-fetch machinery (caching, Chrome
//! escalation for JS-rendered pages like openrouter.ai/benchmarks, egress controls), and its
//! markdown output is exactly what the deterministic parsers in [`crate::scrape_parse`] consume.
//!
//! Contract (from spider-mcp's `server/firecrawl.rs`): request
//! `{"url": "...", "formats": ["markdown"], "timeout": <secs>}`; response either
//! `{"success": true, "data": {"markdown": "…", …}}` or
//! `{"success": false, "error": {"message": "…"}}`. Optional bearer via `Authorization`.

use serde_json::json;

/// A spider-mcp REST endpoint. Build with [`SpiderClient::from_env`] in production wiring; the
/// explicit constructor exists so tests never touch process-global env state.
#[derive(Debug, Clone)]
pub struct SpiderClient {
    http: reqwest::Client,
    base_url: String,
    token: Option<String>,
}

impl SpiderClient {
    pub fn new(base_url: impl Into<String>, token: Option<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into(),
            token,
        }
    }

    /// From the same environment variables spider-mcp's own stdio bridge documents:
    /// `SPIDER_MCP_URL` (required; absent means scraping is disabled) and `SPIDER_MCP_TOKEN`.
    pub fn from_env() -> Option<Self> {
        let base_url = std::env::var("SPIDER_MCP_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())?;
        Some(Self::new(base_url, std::env::var("SPIDER_MCP_TOKEN").ok()))
    }

    /// Fetch one page as markdown. Any failure — transport, non-2xx, error-shaped body,
    /// missing markdown — collapses into one `Err(String)` because every cause has the same
    /// remedy for the caller: try the next source.
    pub async fn scrape_markdown(&self, url: &str, timeout_secs: u64) -> Result<String, String> {
        let response = self
            .http
            .post(format!("{}/v1/scrape", self.base_url.trim_end_matches('/')))
            .apply_bearer(self.token.as_deref())
            .json(&json!({
                "url": url,
                "formats": ["markdown"],
                // Bounded so a wedged Chrome escalation cannot stall model selection forever;
                // spider-mcp treats this as a budget hint, not a hard kill.
                "timeout": timeout_secs,
            }))
            .send()
            .await
            .map_err(|e| format!("scrape transport: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            return Err(format!("scrape HTTP {status}"));
        }

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("scrape body not JSON: {e}"))?;
        if body["success"].as_bool() != Some(true) {
            let message = body["error"]["message"].as_str().unwrap_or("(no message)");
            return Err(format!("scrape failed: {message}"));
        }
        match body["data"]["markdown"].as_str() {
            Some(md) if !md.trim().is_empty() => Ok(md.to_string()),
            _ => Err("scrape returned no markdown".to_string()),
        }
    }
}

trait ApplyBearer {
    fn apply_bearer(self, token: Option<&str>) -> reqwest::RequestBuilder;
}

impl ApplyBearer for reqwest::RequestBuilder {
    fn apply_bearer(self, token: Option<&str>) -> reqwest::RequestBuilder {
        match token {
            Some(t) => self.bearer_auth(t),
            None => self,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn happy_path_returns_the_markdown() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/scrape"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "data": { "markdown": "# Leaderboard\n\n| model | pct |\n|---|---|\n", },
            })))
            .mount(&server)
            .await;

        let md = SpiderClient::new(server.uri(), None)
            .scrape_markdown("https://example.com/leaderboard", 30)
            .await
            .expect("scrape should succeed");
        assert!(md.contains("# Leaderboard"));
    }

    #[tokio::test]
    async fn bearer_token_is_sent_when_configured() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/scrape"))
            .and(header("authorization", "Bearer sekrit"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true, "data": { "markdown": "x" }
            })))
            .mount(&server)
            .await;

        SpiderClient::new(server.uri(), Some("sekrit".into()))
            .scrape_markdown("https://example.com", 30)
            .await
            .expect("authenticated scrape should succeed");
        // `expect` above only passes when the header matched; otherwise 404 from the unmatched mock.
    }

    #[tokio::test]
    async fn error_shaped_success_false_carries_the_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/scrape"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": false, "error": { "message": "chrome escalation budget exhausted" }
            })))
            .mount(&server)
            .await;

        let err = SpiderClient::new(server.uri(), None)
            .scrape_markdown("https://example.com", 30)
            .await
            .expect_err("should fail");
        assert!(err.contains("chrome escalation budget exhausted"), "{err}");
    }

    #[tokio::test]
    async fn empty_markdown_is_a_failure_not_a_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/scrape"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true, "data": { "markdown": "   " }
            })))
            .mount(&server)
            .await;

        assert!(
            SpiderClient::new(server.uri(), None)
                .scrape_markdown("https://example.com", 30)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn http_error_status_is_reported() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/scrape"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let err = SpiderClient::new(server.uri(), None)
            .scrape_markdown("https://example.com", 30)
            .await
            .expect_err("should fail");
        assert!(err.contains("500"), "{err}");
    }

    /// Pins that production wiring reads the documented variables — the one env-mutating test
    /// in this crate, per the workspace rule that process-global state gets exactly one
    /// pinning test and no other readers.
    #[test]
    fn from_env_reads_the_documented_spider_variables() {
        // SAFETY: single-threaded mutation of SPIDER_MCP_URL/SPIDER_MCP_TOKEN in this binary,
        // saved before and restored at scope end by `EnvRestore`. No other test here reads
        // these variables, so there is no concurrent reader to race.
        struct EnvRestore(Option<String>, Option<String>);
        impl Drop for EnvRestore {
            fn drop(&mut self) {
                // SAFETY: same exclusivity argument as above.
                unsafe {
                    match self.0.take() {
                        Some(v) => std::env::set_var("SPIDER_MCP_URL", v),
                        None => std::env::remove_var("SPIDER_MCP_URL"),
                    }
                    match self.1.take() {
                        Some(v) => std::env::set_var("SPIDER_MCP_TOKEN", v),
                        None => std::env::remove_var("SPIDER_MCP_TOKEN"),
                    }
                }
            }
        }
        let _restore = EnvRestore(
            std::env::var("SPIDER_MCP_URL").ok(),
            std::env::var("SPIDER_MCP_TOKEN").ok(),
        );

        // SAFETY: sole writer in this binary for these variables.
        unsafe { std::env::set_var("SPIDER_MCP_URL", "http://127.0.0.1:9") };
        unsafe { std::env::remove_var("SPIDER_MCP_TOKEN") };
        let client = SpiderClient::from_env().expect("a non-empty URL yields a client");
        assert_eq!(client.base_url, "http://127.0.0.1:9");
        assert!(client.token.is_none());

        // SAFETY: sole writer in this binary for these variables.
        unsafe { std::env::set_var("SPIDER_MCP_URL", "") };
        assert!(
            SpiderClient::from_env().is_none(),
            "empty URL counts as unset"
        );
    }
}
