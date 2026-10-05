//! Pick the thinking stamp for one persisted message without growing `sessions.rs`.

use liberado_provider::Role;

/// Assistant messages take the next captured thought. Every other role takes
/// `None` and leaves the queue alone, so tool results do not swallow a thought.
pub(super) fn next_for(
    role: Role,
    pending: &mut std::vec::IntoIter<Option<String>>,
) -> Option<String> {
    if role != Role::Assistant {
        return None;
    }
    pending.next().flatten()
}
