//! Application-service calls for one puppet user.
//!
//! Registration uses the application-service token and no `user_id` query.
//! Every later call masquerades as the puppet (`?user_id=`). `M_USER_IN_USE`
//! means the account is already there.

use liberado_messaging::MessagingError;
use serde_json::json;

use crate::client::MatrixClient;

/// `POST /_matrix/client/v3/register` as the application service.
pub async fn register_localpart(
    client: &MatrixClient,
    localpart: &str,
) -> Result<(), MessagingError> {
    let body = json!({
        "type": "m.login.application_service",
        "username": localpart,
    });
    client.post_service(&["register"], &body).await?;
    Ok(())
}

/// True when registration found an account that already exists.
pub fn user_in_use(error: &MessagingError) -> bool {
    error.to_string().contains("M_USER_IN_USE")
}

/// Register, or continue when the homeserver already has this localpart.
pub async fn register_or_existing(
    client: &MatrixClient,
    localpart: &str,
) -> Result<(), MessagingError> {
    match register_localpart(client, localpart).await {
        Ok(()) => Ok(()),
        Err(error) if user_in_use(&error) => Ok(()),
        Err(error) => Err(error),
    }
}

/// `PUT /profile/{user}/displayname`.
pub async fn set_display_name(
    client: &MatrixClient,
    display_name: &str,
) -> Result<(), MessagingError> {
    client
        .put(
            &["profile", client.user_id(), "displayname"],
            &json!({ "displayname": display_name }),
        )
        .await?;
    Ok(())
}

/// `PUT /user/{user}/account_data/m.direct` for the one owner DM.
pub async fn set_direct_room(
    client: &MatrixClient,
    owner_mxid: &str,
    room_id: &str,
) -> Result<(), MessagingError> {
    client
        .put(
            &["user", client.user_id(), "account_data", "m.direct"],
            &json!({ owner_mxid: [room_id] }),
        )
        .await?;
    Ok(())
}

/// `PUT` `m.room.name`.
pub async fn set_room_name(
    client: &MatrixClient,
    room_id: &str,
    name: &str,
) -> Result<(), MessagingError> {
    client
        .put_state(room_id, "m.room.name", "", &json!({ "name": name }))
        .await
}

/// `POST /rooms/{id}/leave`.
pub async fn leave_room(client: &MatrixClient, room_id: &str) -> Result<(), MessagingError> {
    client
        .post(&["rooms", room_id, "leave"], &json!({}))
        .await?;
    Ok(())
}

/// `POST /rooms/{id}/invite`.
pub async fn invite_user(
    client: &MatrixClient,
    room_id: &str,
    user_id: &str,
) -> Result<(), MessagingError> {
    client
        .post(
            &["rooms", room_id, "invite"],
            &json!({ "user_id": user_id }),
        )
        .await?;
    Ok(())
}

/// Leave failed because the puppet is already gone.
pub fn already_left(error: &MessagingError) -> bool {
    let text = error.to_string();
    text.contains("M_FORBIDDEN") || text.contains("M_NOT_FOUND") || text.contains("M_UNKNOWN")
}

/// Invite failed because the owner is already in the room, or the server refused a repeat invite.
pub fn already_invited(error: &MessagingError) -> bool {
    let text = error.to_string();
    text.contains("already") || text.contains("M_FORBIDDEN")
}

#[cfg(test)]
#[path = "puppet_tests.rs"]
mod tests;
