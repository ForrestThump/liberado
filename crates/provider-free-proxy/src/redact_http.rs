//! Reqwest errors include the request URL. Strip it before a log or a client sees the text.

pub(crate) fn transport(err: reqwest::Error) -> String {
    liberado_common::redact_secrets(&err.without_url().to_string())
}

pub(crate) fn text(detail: impl std::fmt::Display) -> String {
    liberado_common::redact_secrets(&detail.to_string())
}
