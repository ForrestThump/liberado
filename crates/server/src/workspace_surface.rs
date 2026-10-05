//! Names the boot log may claim for the private workspace tools.
//!
//! The names are present only when the workspace root can be opened. The boot
//! log then matches the tools a new session can receive. A root that cannot be
//! created leaves the list empty, the same way a failed open withholds the tools.

/// Tool names to append, or an empty list when this process will not offer them.
pub(crate) fn listed_names(offered: bool) -> Vec<String> {
    if !offered {
        return Vec::new();
    }
    liberado_main_agent::WORKSPACE_TOOL_NAMES
        .iter()
        .copied()
        .map(str::to_string)
        .collect()
}
