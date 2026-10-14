//! Strip secrets from text before it is logged or returned to a client.
//!
//! Reqwest puts the full request URL in [`std::fmt::Display`] for `reqwest::Error`.
//! Callers that have the error value must pass `err.without_url()` first. This
//! function is the second pass: it still removes a Telegram bot token, URL
//! userinfo, or a secret query value when the URL text is already a string.

const REDACTED: &str = "<redacted>";

/// Remove secret material from `input`.
///
/// A second call returns the same string. Benign prose is unchanged.
pub fn redact_secrets(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while !rest.is_empty() {
        if let Some((taken, replacement)) = userinfo_rewrite(rest) {
            out.push_str(&replacement);
            rest = &rest[taken..];
            continue;
        }
        if let Some((taken, replacement)) = query_rewrite(rest) {
            out.push_str(&replacement);
            rest = &rest[taken..];
            continue;
        }
        if let Some(taken) = bot_token_len(rest) {
            out.push_str("bot");
            out.push_str(REDACTED);
            rest = &rest[taken..];
            continue;
        }
        if let Some(taken) = bare_token_len(rest) {
            out.push_str(REDACTED);
            rest = &rest[taken..];
            continue;
        }
        let Some(ch) = rest.chars().next() else {
            break;
        };
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

/// `bot` + at least six digits + `:` + at least ten token characters.
///
/// Telegram's bot token is `<numeric id>:<secret>` and the request path is
/// `/bot<id>:<secret>/<method>`. Six digits and ten secret characters stay
/// above ordinary words such as `approve:prop-1`.
fn bot_token_len(input: &str) -> Option<usize> {
    if !starts_with_ignore_ascii_case(input, "bot") {
        return None;
    }
    let rest = &input[3..];
    let digits = leading_digits(rest);
    if digits < 6 {
        return None;
    }
    let after_digits = &rest[digits..];
    if !after_digits.starts_with(':') {
        return None;
    }
    let secret = secret_run(&after_digits[1..]);
    if secret < 10 {
        return None;
    }
    Some(3 + digits + 1 + secret)
}

/// A bare `<digits>:<secret>` with a long secret, when the `bot` prefix is absent.
fn bare_token_len(input: &str) -> Option<usize> {
    let digits = leading_digits(input);
    if digits < 6 {
        return None;
    }
    let after_digits = &input[digits..];
    if !after_digits.starts_with(':') {
        return None;
    }
    let secret = secret_run(&after_digits[1..]);
    if secret < 20 {
        return None;
    }
    Some(digits + 1 + secret)
}

/// `scheme://userinfo@` becomes `scheme://<redacted>@`.
fn userinfo_rewrite(input: &str) -> Option<(usize, String)> {
    let scheme_end = input.find("://")?;
    if scheme_end == 0 || scheme_end > 16 {
        return None;
    }
    if !input[..scheme_end]
        .bytes()
        .all(|byte| byte.is_ascii_alphabetic())
    {
        return None;
    }
    let after = scheme_end + 3;
    let rest = input.get(after..)?;
    let at = rest.find('@')?;
    let userinfo = &rest[..at];
    if userinfo.is_empty()
        || userinfo
            .bytes()
            .any(|byte| byte == b'/' || byte == b'?' || byte == b'#' || byte.is_ascii_whitespace())
    {
        return None;
    }
    let total = after + at + 1;
    let mut replacement = String::new();
    replacement.push_str(&input[..after]);
    replacement.push_str(REDACTED);
    replacement.push('@');
    Some((total, replacement))
}

/// `?key=value` / `&key=value` for a secret query name. The name stays; the value goes.
fn query_rewrite(input: &str) -> Option<(usize, String)> {
    let delim = *input.as_bytes().first()?;
    if delim != b'?' && delim != b'&' {
        return None;
    }
    let rest = &input[1..];
    let eq = rest.find('=')?;
    let key = &rest[..eq];
    if key.is_empty()
        || key
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || byte == b'_'))
        || !is_secret_key(key)
    {
        return None;
    }
    let value_start = 1 + eq + 1;
    let value_len: usize = input[value_start..]
        .chars()
        .take_while(|ch| !ends_query_value(*ch))
        .map(char::len_utf8)
        .sum();
    let total = value_start + value_len;
    let mut replacement = String::new();
    replacement.push(delim as char);
    replacement.push_str(key);
    replacement.push('=');
    replacement.push_str(REDACTED);
    Some((total, replacement))
}

fn ends_query_value(ch: char) -> bool {
    matches!(ch, '&' | '#' | '"' | '\'' | ')' | ',') || ch.is_whitespace()
}

fn is_secret_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "token"
            | "api_key"
            | "apikey"
            | "access_token"
            | "key"
            | "secret"
            | "bot_token"
            | "password"
    )
}

fn starts_with_ignore_ascii_case(input: &str, prefix: &str) -> bool {
    let bytes = input.as_bytes();
    let prefix = prefix.as_bytes();
    bytes.len() >= prefix.len() && bytes[..prefix.len()].eq_ignore_ascii_case(prefix)
}

fn leading_digits(input: &str) -> usize {
    input
        .bytes()
        .take_while(|byte| byte.is_ascii_digit())
        .count()
}

fn secret_run(input: &str) -> usize {
    input
        .bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'-')
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw";

    #[test]
    fn telegram_url_loses_the_bot_token() {
        let raw = format!("https://api.telegram.org/bot{TOKEN}/getUpdates");
        let clean = redact_secrets(&raw);
        assert!(!clean.contains("123456789"), "{clean}");
        assert!(
            !clean.contains("AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw"),
            "{clean}"
        );
        assert!(clean.contains("bot<redacted>"), "{clean}");
        assert!(clean.contains("/getUpdates"), "{clean}");
        assert_eq!(redact_secrets(&clean), clean);
    }

    #[test]
    fn userinfo_and_secret_query_values_are_removed() {
        let raw = "https://user:s3cret@example.com/v1?api_key=sk-test&page=2";
        let clean = redact_secrets(raw);
        assert!(!clean.contains("s3cret"), "{clean}");
        assert!(!clean.contains("sk-test"), "{clean}");
        assert!(
            clean.contains("https://<redacted>@example.com/v1"),
            "{clean}"
        );
        assert!(clean.contains("api_key=<redacted>"), "{clean}");
        assert!(clean.contains("page=2"), "{clean}");
        assert_eq!(redact_secrets(&clean), clean);
    }

    #[test]
    fn prose_and_callback_data_stay() {
        let raw = "The token is fine. callback approve:prop-1 and bot are words.";
        assert_eq!(redact_secrets(raw), raw);
    }
}
