//! Effects a tool may perform.
//!
//! A tool lists each effect it performs. `Write` does not include `Delete`.
//! `Read` does not include `Write`. An effect that is absent from the
//! declaration is denied.

use serde::{Deserialize, Serialize};

/// One effect a tool declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Read data. No mutation.
    Read,
    /// Create or update data inside the system.
    Write,
    /// Remove data.
    Delete,
    /// Leave the system. A network call or an external service.
    External,
    /// Operator-only control. Chat sessions do not receive this effect.
    Admin,
}
