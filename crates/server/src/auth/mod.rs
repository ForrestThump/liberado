//! API authentication: who may reach the control plane, from where, and as whom.
//!
//! Before this module the daemon bound `0.0.0.0:4201`, served `CorsLayer::permissive()`, and
//! checked no caller identity anywhere. `POST /api/approvals/{id}/resolve` — the one route
//! whose whole purpose is "a human decided" — was reachable from any host that could route to
//! the box, from any browser tab, and from the shell a coding run opens.
//!
//! Four pieces close that:
//!
//! - [`bind`] makes the default listen address loopback and refuses a wider one when no
//!   credential exists to protect it.
//! - [`tokens`] resolves `[api]` credential references into tokens once at boot, and matches a
//!   presented one in constant time. Two kinds exist: per-client tokens, which authenticate,
//!   and the human token, which is the only one that carries owner authority.
//! - [`sessions`] exchanges the human token for an `HttpOnly; SameSite=Strict` cookie so the
//!   browser does not hold the token itself.
//! - [`guard`] is the request-time layer: 401 without a credential, 403 for a client on an
//!   owner-only route, and the CSRF and same-origin checks that a cookie credential needs and
//!   a bearer token does not.
//!
//! The split between "authenticated" and "owner" is the load-bearing one. An agent needs to
//! post a chat turn and read its own approvals; nothing an agent does should be able to decide
//! them.

pub mod bind;
pub mod guard;
pub mod sessions;
pub mod tokens;

pub use bind::{BindPlan, ensure_bind_is_credentialed, resolve_bind};
pub use guard::{ApiGuard, require_credential, require_owner};
pub use tokens::ApiAuth;
