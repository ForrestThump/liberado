//! Split from `types.rs` for module-health boundaries.

use super::*;

/// Exactly the [`COMPACTION_TAIL_AUTHOR`] identity counts as a tail copy — not the marker
/// itself (`COMPACTION_AUTHOR`), not any role author, not look-alike names. Readers rely on
/// this to skip duplicated content without dropping real messages.
#[test]
fn only_the_compaction_tail_author_is_a_tail_copy() {
    assert!(Author::Named(COMPACTION_TAIL_AUTHOR.into()).is_compaction_tail_copy());

    let not_copies = [
        Author::System,
        Author::User,
        Author::Assistant,
        Author::Tool,
        // The compaction *marker* precedes the copies and is model-visible on its own;
        // conflating it with the tail would hide the summary from readers that skip copies.
        Author::Named(COMPACTION_AUTHOR.into()),
        Author::Named("user".into()),
    ];
    for author in not_copies {
        assert!(
            !author.is_compaction_tail_copy(),
            "{author:?} is not a tail copy"
        );
    }
}
