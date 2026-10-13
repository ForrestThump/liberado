//! A failed Telegram call must not carry the bot token into the error string.

use liberado_messaging::MessagingChannel;
use liberado_notify::TelegramNotifier;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw";

fn assert_token_absent(text: &str) {
    assert!(!text.contains(TOKEN), "{text}");
    assert!(
        !text.contains("AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw"),
        "{text}"
    );
    assert!(!text.contains("123456789:"), "{text}");
}

#[tokio::test]
async fn send_text_transport_error_hides_the_bot_token() {
    let notifier = TelegramNotifier::new(TOKEN, "42")
        .with_api_base("http://127.0.0.1:1/bot")
        .with_poll_tuning(1, 0);
    let err = notifier.send_text("hello").await.unwrap_err();
    assert!(err.0.contains("Telegram request failed"), "{}", err.0);
    assert_token_absent(&err.0);
}

#[tokio::test]
async fn api_error_body_hides_an_echoed_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_string(format!("refused {TOKEN}")))
        .mount(&server)
        .await;
    let notifier =
        TelegramNotifier::new(TOKEN, "42").with_api_base(format!("{}/bot", server.uri()));
    let err = notifier.send_text("hello").await.unwrap_err();
    assert!(err.0.contains("Telegram API error"), "{}", err.0);
    assert!(err.0.contains("<redacted>"), "{}", err.0);
    assert_token_absent(&err.0);
}

#[tokio::test]
async fn getupdates_decode_error_hides_the_bot_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not-json"))
        .mount(&server)
        .await;
    let notifier = TelegramNotifier::new(TOKEN, "42")
        .with_api_base(format!("{}/bot", server.uri()))
        .with_poll_tuning(1, 0);
    let mut cursor = String::new();
    let err = notifier.receive(&mut cursor).await.unwrap_err();
    assert!(err.0.contains("Telegram getUpdates decode"), "{}", err.0);
    assert_token_absent(&err.0);
}
