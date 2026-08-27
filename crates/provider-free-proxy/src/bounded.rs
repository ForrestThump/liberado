//! Bounded response-body reads.
//!
//! Every body this proxy buffers from a *peer we do not fully control* (spider-mcp's scrape
//! output, an upstream's error payloads) goes through `read_capped`: a peer (or anything
//! impersonating one) must not choose this process's memory footprint. Successful completion
//! streams are relayed chunk-by-chunk and never buffered, so they need no cap.

/// Read a response body fully, refusing anything larger than `cap` bytes.
///
/// The content-length header fails fast when present, but the byte-count check is authoritative:
/// headers are claims, chunks are facts.
pub(crate) async fn read_capped(
    mut response: reqwest::Response,
    what: &str,
    cap: usize,
) -> Result<Vec<u8>, String> {
    if let Some(len) = response.content_length()
        && len as usize > cap
    {
        return Err(format!(
            "{what} body is {} bytes, over the {cap}-byte cap",
            len
        ));
    }
    let mut out: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("{what} transport: {e}"))?
    {
        if out.len().saturating_add(chunk.len()) > cap {
            return Err(format!("{what} body exceeded the {cap}-byte cap"));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn bodies_under_the_cap_read_fully() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(200).set_body_string("hello"))
            .mount(&server)
            .await;
        let resp = reqwest::get(server.uri() + "/x").await.expect("get");
        let bytes = read_capped(resp, "probe", 1024).await.expect("reads");
        assert_eq!(bytes, b"hello");
    }

    #[tokio::test]
    async fn bodies_over_the_cap_are_refused_not_truncated() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/big"))
            .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(4096)))
            .mount(&server)
            .await;
        let resp = reqwest::get(server.uri() + "/big").await.expect("get");
        let err = read_capped(resp, "probe", 1024)
            .await
            .expect_err("must refuse");
        assert!(err.contains("cap"), "{err}");
    }

    /// `>` vs `>=`/`==` at the cap: a body of exactly `cap` bytes is admitted, including when
    /// the content-length header claims that exact size.
    #[tokio::test]
    async fn a_body_exactly_at_the_cap_is_admitted() {
        let server = MockServer::start().await;
        let body = "x".repeat(1024);
        Mock::given(method("GET"))
            .and(path("/exact"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let resp = reqwest::get(server.uri() + "/exact").await.expect("get");
        let bytes = read_capped(resp, "probe", 1024)
            .await
            .expect("exactly the cap is not over it");
        assert_eq!(bytes.len(), 1024);
    }

    // A deliberately lying content-length cannot be served by wiremock/hyper (it refuses to
    // frame a body that contradicts its own header), so the chunk-count check above has no
    // direct fixture; it remains as defense-in-depth against peers that stream past their
    // claimed length.
}
