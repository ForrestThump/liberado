//! Create Project modal — opens from the Projects section's `+` button.
//!
//! The dialog matches the screenshot: a card with a close X, a single text field labelled
//! "Project name", and a footer with an "Add" secondary action and a "Create" primary action.
//! On the host the actions are placeholder callbacks; on the wire the daemon will eventually
//! back the `Create` hit.

use dioxus::prelude::*;

#[component]
pub fn ProjectsDialog(
    on_cancel: EventHandler<()>,
    on_create: EventHandler<String>,
) -> Element {
    let mut name = use_signal(String::new);
    let mut error = use_signal(|| None::<&'static str>);

    // The Enter-to-submit and footer-Create paths both call this. `use_callback` returns a
    // `Callback<()>` that's `Copy` under the hood, so the same handle drives both events.
    let submit = use_callback(move |_: ()| {
        let trimmed = name.read().trim().to_string();
        if trimmed.is_empty() {
            error.set(Some("Enter a project name."));
            return;
        }
        on_create(trimmed);
    });

    rsx! {
        div {
            class: "projects-dialog-backdrop",
            role: "dialog",
            "aria-modal": "true",
            "aria-labelledby": "projects-dialog-title",
            onclick: move |evt: MouseEvent| {
                // Clicking the backdrop dismisses; clicking the card itself does not.
                evt.stop_propagation();
            },
            div {
                class: "projects-dialog-card",
                onclick: move |evt: MouseEvent| evt.stop_propagation(),
                header {
                    class: "projects-dialog-header",
                    h2 {
                        id: "projects-dialog-title",
                        class: "projects-dialog-title",
                        "Create Project"
                    }
                    button {
                        class: "projects-dialog-close",
                        r#type: "button",
                        title: "Close",
                        "aria-label": "Close",
                        onclick: move |_| on_cancel(()),
                        "×"
                    }
                }
                div {
                    class: "projects-dialog-body",
                    label {
                        class: "projects-dialog-label",
                        r#for: "projects-dialog-name",
                        "Project name"
                    }
                    input {
                        id: "projects-dialog-name",
                        class: if error().is_some() {
                            "projects-dialog-input error"
                        } else {
                            "projects-dialog-input"
                        },
                        r#type: "text",
                        placeholder: "New project",
                        autofocus: true,
                        value: "{name}",
                        oninput: move |e| {
                            name.set(e.value());
                            error.set(None);
                        },
                        onkeydown: move |e: Event<KeyboardData>| {
                            if matches!(e.key(), Key::Enter) {
                                submit(());
                            }
                        },
                    }
                    if let Some(message) = error() {
                        p {
                            class: "projects-dialog-error",
                            "{message}"
                        }
                    }
                }
                footer {
                    class: "projects-dialog-footer",
                    button {
                        class: "projects-dialog-add",
                        r#type: "button",
                        onclick: move |_| on_cancel(()),
                        span {
                            class: "projects-dialog-plus",
                            "aria-hidden": "true",
                            "+"
                        }
                        "Add"
                    }
                    button {
                        class: "projects-dialog-create",
                        r#type: "button",
                        onclick: move |evt: MouseEvent| {
                            evt.prevent_default();
                            submit(());
                        },
                        "Create"
                    }
                }
            }
        }
    }
}
