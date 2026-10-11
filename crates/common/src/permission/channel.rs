//! Channel identity and where a permission request is shown.
//!
//! Stored names stay the strings already on disk (`telegram`, `web`, `background`, `webui`).

use serde::{Deserialize, Serialize};

/// A chat channel a Liberado session can be bound to.
///
/// `Matrix` is reserved and unused. Stored names stay the snake-case strings already on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Telegram,
    Matrix,
}

impl ChannelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Telegram => "telegram",
            Self::Matrix => "matrix",
        }
    }

    /// Human label for replies and session titles (`Telegram`).
    pub fn label(self) -> &'static str {
        match self {
            Self::Telegram => "Telegram",
            Self::Matrix => "Matrix",
        }
    }
}

/// Where the request was raised. Drives which surfaces show it.
///
/// Unsigned, like [`GrantScope`](crate::GrantScope): it is routing metadata, not part of the
/// proposal signature. Stored as a string. A channel origin writes [`ChannelKind::as_str`], so
/// Telegram still writes `telegram`. `web` and `background` are unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOrigin {
    /// A channel-bound chat. Show that channel's message and a WebUI card in the same session.
    Channel(ChannelKind),
    /// Any other human chat (WebUI, an agent opened in the WebUI). Card only.
    Web,
    /// Cron, a goal, or another run with no human chat. The configured channel stays the push.
    Background,
}

impl ApprovalOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Channel(kind) => kind.as_str(),
            Self::Web => "web",
            Self::Background => "background",
        }
    }

    pub fn channel(self) -> Option<ChannelKind> {
        match self {
            Self::Channel(kind) => Some(kind),
            _ => None,
        }
    }
}

impl Serialize for ApprovalOrigin {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ApprovalOrigin {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        parse_origin(&raw).ok_or_else(|| {
            serde::de::Error::unknown_variant(&raw, &["telegram", "matrix", "web", "background"])
        })
    }
}

fn parse_origin(raw: &str) -> Option<ApprovalOrigin> {
    match raw {
        "web" => Some(ApprovalOrigin::Web),
        "background" => Some(ApprovalOrigin::Background),
        other => parse_channel(other).map(ApprovalOrigin::Channel),
    }
}

fn parse_channel(raw: &str) -> Option<ChannelKind> {
    match raw {
        "telegram" => Some(ChannelKind::Telegram),
        "matrix" => Some(ChannelKind::Matrix),
        _ => None,
    }
}

/// Which human surface recorded the decision. Audit only.
///
/// Stored as a string. A channel decision writes [`ChannelKind::as_str`], so Telegram still
/// writes `telegram`. `webui` is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionVia {
    Channel(ChannelKind),
    Webui,
}

impl DecisionVia {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Channel(kind) => kind.as_str(),
            Self::Webui => "webui",
        }
    }
}

impl Serialize for DecisionVia {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DecisionVia {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        parse_via(&raw).ok_or_else(|| {
            serde::de::Error::unknown_variant(&raw, &["telegram", "matrix", "webui"])
        })
    }
}

fn parse_via(raw: &str) -> Option<DecisionVia> {
    match raw {
        "webui" => Some(DecisionVia::Webui),
        other => parse_channel(other).map(DecisionVia::Channel),
    }
}

/// Where a newly raised request is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionRoute {
    /// Channel message and a card in `session_id`.
    ChannelAndWeb {
        channel: ChannelKind,
        session_id: String,
    },
    /// Card in `session_id` only. No channel message.
    WebOnly { session_id: String },
    /// No human chat of its own. Keep the channel push. `bound_session` is the WebUI
    /// conversation that also shows the card.
    Background { bound_session: Option<String> },
}

/// One session bound to a channel, as the risk gate reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionChannel {
    pub session_id: String,
    pub channel: ChannelKind,
}

/// Channel of `session_id`, if that session is bound.
pub fn channel_for_session(bindings: &[SessionChannel], session_id: &str) -> Option<ChannelKind> {
    bindings
        .iter()
        .find(|binding| binding.session_id == session_id)
        .map(|binding| binding.channel)
}

/// `background` is the unattended path (cron, dispatcher, goal) even when it carries an id.
///
/// `bound_channel` is set when `session_id` itself is a channel-bound chat. `background_session`
/// is the chat that also shows a background card (the bound channel session).
pub fn route_permission(
    session_id: Option<&str>,
    bound_channel: Option<ChannelKind>,
    background_session: Option<&str>,
    background: bool,
) -> PermissionRoute {
    let Some(session_id) = session_id.filter(|_| !background) else {
        return background_route(background_session);
    };
    match bound_channel {
        Some(channel) => PermissionRoute::ChannelAndWeb {
            channel,
            session_id: session_id.to_string(),
        },
        None => PermissionRoute::WebOnly {
            session_id: session_id.to_string(),
        },
    }
}

fn background_route(background_session: Option<&str>) -> PermissionRoute {
    PermissionRoute::Background {
        bound_session: background_session.map(str::to_string),
    }
}
