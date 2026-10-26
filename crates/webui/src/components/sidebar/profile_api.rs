//! Profile rename and delete for agent rows.
//!
//! `PATCH` and `DELETE /api/profiles/{name}` change the live profile name. They do not
//! delete the conversation. Chat rows keep `/api/conversations/{id}`.

use dioxus::prelude::*;

use super::ConversationPage;

pub(super) fn profile_from_page(page: &ConversationPage, id: &str) -> Option<String> {
    page.profiles.get(id).cloned()
}

pub(super) fn mutation_error(action: &str, status: u16, body: &str) -> String {
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.as_str())
                .map(str::to_string)
        });
    match detail {
        Some(text) => text,
        None => format!("{action} failed: HTTP {status}"),
    }
}

async fn send_profile(
    patch: bool,
    api_base: &str,
    name: &str,
    body: Option<serde_json::Value>,
    action: &str,
) -> Result<(), String> {
    let url = format!("{api_base}/api/profiles/{name}");
    let resp = if patch {
        crate::http::patch_json(&url, &body.expect("patch needs a body"))
            .await
            .map_err(|e| format!("Failed to reach daemon: {e}"))?
    } else {
        crate::http::delete(&url)
            .await
            .map_err(|e| format!("Failed to reach daemon: {e}"))?
    };
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        Ok(())
    } else {
        let text = resp.text().await.unwrap_or_default();
        Err(mutation_error(action, status, &text))
    }
}

async fn rename_profile(api_base: &str, name: &str, new_name: &str) -> Result<(), String> {
    send_profile(
        true,
        api_base,
        name,
        Some(serde_json::json!({ "name": new_name })),
        "Rename",
    )
    .await
}

async fn delete_profile(api_base: &str, name: &str) -> Result<(), String> {
    send_profile(false, api_base, name, None, "Delete").await
}

fn note_result(
    result: Result<(), String>,
    menu_for: &mut Signal<Option<String>>,
    action_error: &mut Signal<Option<String>>,
) -> bool {
    match result {
        Ok(()) => {
            menu_for.set(None);
            action_error.set(None);
            true
        }
        Err(error) => {
            action_error.set(Some(error));
            false
        }
    }
}

/// Agent-row callbacks. `profile_of` maps a conversation id to its live profile name.
#[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
pub(super) fn use_profile_actions(
    api_base: String,
    menu_for: Signal<Option<String>>,
    action_error: Signal<Option<String>>,
    on_saved: EventHandler<()>,
    profile_of: impl Fn(String) -> Option<String> + Copy + 'static,
) -> (EventHandler<(String, String)>, EventHandler<String>) {
    let rename_base = api_base.clone();
    let mut rename_menu = menu_for;
    let mut rename_error = action_error;
    let rename_saved = on_saved;
    let rename = use_callback(move |(id, new_name): (String, String)| {
        let Some(profile) = profile_of(id) else {
            rename_error.set(Some("This agent has no profile to rename.".into()));
            return;
        };
        let base = rename_base.clone();
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            let result = rename_profile(&base, &profile, &new_name).await;
            if note_result(result, &mut rename_menu, &mut rename_error) {
                rename_saved.call(());
            }
        });
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (
                base,
                profile,
                new_name,
                rename_menu,
                rename_error,
                rename_saved,
            );
        }
    });
    let mut delete_menu = menu_for;
    let mut delete_error = action_error;
    let delete_saved = on_saved;
    let delete = use_callback(move |id: String| {
        let Some(profile) = profile_of(id) else {
            delete_error.set(Some("This agent has no profile to delete.".into()));
            return;
        };
        let base = api_base.clone();
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            let result = delete_profile(&base, &profile).await;
            if note_result(result, &mut delete_menu, &mut delete_error) {
                delete_saved.call(());
            }
        });
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (base, profile, delete_menu, delete_error, delete_saved);
        }
    });
    (rename, delete)
}

#[cfg(test)]
mod tests {
    use super::mutation_error;

    #[test]
    fn a_daemon_error_string_is_shown_as_written() {
        let body = r#"{"error":"session profile 'operator' is built in (default or Agent Creator) and cannot be changed"}"#;
        let text = mutation_error("Delete", 403, body);
        assert!(text.contains("operator"), "{text}");
        assert!(text.contains("built in"), "{text}");
    }

    #[test]
    fn a_body_without_error_keeps_the_status() {
        assert_eq!(
            mutation_error("Rename", 500, "nope"),
            "Rename failed: HTTP 500"
        );
    }
}
