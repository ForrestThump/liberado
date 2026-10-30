//! Base64url without padding, as Web Push uses it on the wire.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

pub(crate) fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub(crate) fn decode(text: &str) -> Result<Vec<u8>, ()> {
    let trimmed = text.trim().trim_end_matches('=');
    URL_SAFE_NO_PAD.decode(trimmed).map_err(|_| ())
}
