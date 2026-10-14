//! Constant-time comparison for the homeserver token.
//!
//! A short-circuiting equality check would leak the matching prefix through timing.
//! This walks the longer input and folds every byte into one difference.

/// `true` when `expected` and `presented` are the same byte string.
pub fn tokens_match(expected: &str, presented: &str) -> bool {
    let left = expected.as_bytes();
    let right = presented.as_bytes();
    let mut diff = left.len() ^ right.len();
    let n = left.len().max(right.len());
    for i in 0..n {
        let a = left.get(i).copied().unwrap_or(0);
        let b = right.get(i).copied().unwrap_or(0);
        diff |= usize::from(a ^ b);
    }
    diff == 0
}

/// Bearer token from an `Authorization` header value, if the scheme is Bearer.
pub fn bearer_token(header: &str) -> Option<&str> {
    let header = header.trim();
    let rest = header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))?;
    let rest = rest.trim();
    (!rest.is_empty()).then_some(rest)
}

/// Why a homeserver request was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    /// No token was presented.
    Unknown,
    /// A token was presented and it did not match.
    Forbidden,
}

/// Check the homeserver token. The header wins when it carries a Bearer token.
/// Otherwise the legacy `access_token` query value is accepted.
pub fn check_token(
    expected: &str,
    authorization: Option<&str>,
    query_token: Option<&str>,
) -> Result<(), AuthError> {
    let presented = presented_token(authorization, query_token).ok_or(AuthError::Unknown)?;
    if tokens_match(expected, presented) {
        Ok(())
    } else {
        Err(AuthError::Forbidden)
    }
}

fn presented_token<'a>(
    authorization: Option<&'a str>,
    query_token: Option<&'a str>,
) -> Option<&'a str> {
    if let Some(token) = authorization.and_then(bearer_token) {
        return Some(token);
    }
    query_token.map(str::trim).filter(|token| !token.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{AuthError, bearer_token, check_token, tokens_match};

    #[test]
    fn equal_tokens_match_and_prefixes_do_not() {
        assert!(tokens_match("hs-token", "hs-token"));
        assert!(!tokens_match("hs-token", "hs-token-extra"));
        assert!(!tokens_match("hs-token", "hs-tokem"));
        assert!(!tokens_match("hs-token", ""));
        assert!(tokens_match("", ""));
    }

    #[test]
    fn bearer_parsing_accepts_either_case() {
        assert_eq!(bearer_token("Bearer secret"), Some("secret"));
        assert_eq!(bearer_token("bearer secret"), Some("secret"));
        assert_eq!(bearer_token("Basic secret"), None);
        assert_eq!(bearer_token("Bearer   "), None);
    }

    #[test]
    fn missing_token_is_unknown_and_a_wrong_token_is_forbidden() {
        assert_eq!(check_token("hs", None, None), Err(AuthError::Unknown));
        assert_eq!(
            check_token("hs", Some("Bearer no"), None),
            Err(AuthError::Forbidden)
        );
        assert_eq!(check_token("hs", Some("Bearer hs"), None), Ok(()));
        assert_eq!(check_token("hs", None, Some("hs")), Ok(()));
        assert_eq!(
            check_token("hs", Some("Bearer other"), Some("hs")),
            Err(AuthError::Forbidden)
        );
    }
}
