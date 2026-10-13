//! Format Telegram HTTP failures so the bot token cannot reach a log or a caller.
//!
//! The request URL is `…/bot<token>/<method>`. Reqwest includes that URL in the
//! error's Display. [`line`] drops the URL, then runs [`liberado_common::redact_secrets`]
//! so a token that survives in the message text is still removed.

use liberado_messaging::MessagingError;

pub(crate) fn transport(context: &str, err: reqwest::Error) -> MessagingError {
    MessagingError(line(context, err))
}

/// The text the getUpdates warning logs, and the body of every [`transport`] error.
///
/// `without_url` takes the error by value. A borrow cannot drop the URL.
pub(crate) fn line(context: &str, err: reqwest::Error) -> String {
    format!(
        "{context}: {}",
        liberado_common::redact_secrets(&err.without_url().to_string())
    )
}

pub(crate) fn api_body(body: &str) -> MessagingError {
    MessagingError(liberado_common::redact_secrets(&format!(
        "Telegram API error: {body}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw";

    #[tokio::test]
    async fn line_drops_the_bot_token_from_a_reqwest_error() {
        let url = format!("http://127.0.0.1:1/bot{TOKEN}/getUpdates");
        let err = reqwest::Client::new().get(url).send().await.unwrap_err();
        let text = line("Telegram getUpdates error", err);
        assert!(text.starts_with("Telegram getUpdates error:"), "{text}");
        assert!(!text.contains(TOKEN), "{text}");
        assert!(
            !text.contains("AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw"),
            "{text}"
        );
        assert!(!text.contains("123456789:"), "{text}");
    }
}
