//! Percent-encoding for Matrix path segments and query values.
//!
//! Room ids, event ids, and user ids contain `!`, `$`, `@`, and `:`. Those must be encoded
//! in a URL path. The unreserved set is the RFC 3986 set.

/// Encode one URL path segment or query value.
pub fn encode_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        if is_unreserved(byte) {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(hex(byte >> 4));
            out.push(hex(byte & 0x0f));
        }
    }
    out
}

fn is_unreserved(byte: u8) -> bool {
    matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~')
}

fn hex(nibble: u8) -> char {
    char::from(b"0123456789ABCDEF"[nibble as usize])
}

#[cfg(test)]
mod tests {
    use super::encode_segment;

    #[test]
    fn room_and_user_ids_are_encoded() {
        assert_eq!(
            encode_segment("!room:matrix.homelab.local"),
            "%21room%3Amatrix.homelab.local"
        );
        assert_eq!(
            encode_segment("@liberado:matrix.homelab.local"),
            "%40liberado%3Amatrix.homelab.local"
        );
        assert_eq!(encode_segment("$evt"), "%24evt");
    }

    #[test]
    fn plain_segments_stay_plain() {
        assert_eq!(encode_segment("m.room.message"), "m.room.message");
        assert_eq!(encode_segment("v3"), "v3");
    }
}
