//! Split from `search.rs` for module-health boundaries.

use super::*;

/// The documented default page size is 20; a stub default would silently shrink (or
/// zero out) every search that does not pass `limit` explicitly.
#[test]
fn the_default_search_limit_is_twenty() {
    let query: SearchQuery = serde_urlencoded::from_str("q=hello").expect("minimal query");
    assert_eq!(query.limit, 20);
    assert!(!query.regex);
}
