//! The path the shell opens, and where Back goes when no layer is open.

use dioxus::prelude::*;

use crate::components::project_data::{ProjectPath, project_route_from_path};
use crate::components::sidebar::Route;
use crate::notification_route;

pub(crate) struct BootRoute {
    pub(crate) route: Route,
    pub(crate) project: Option<String>,
}

pub(crate) fn boot_route(path: &str) -> Option<BootRoute> {
    if let Some(route) = notification_route::route_from_path(path) {
        return Some(BootRoute {
            route,
            project: None,
        });
    }
    let project = project_route_from_path(path)?;
    let (route, id) = match project {
        ProjectPath::List => (Route::Projects, None),
        ProjectPath::Detail(id) => (Route::ProjectDetail, Some(id)),
    };
    Some(BootRoute { route, project: id })
}

pub(crate) fn apply_boot_route(
    path: &str,
    mut route: Signal<Route>,
    mut focus: Signal<Option<String>>,
) {
    let Some(boot) = boot_route(path) else {
        return;
    };
    if let Some(id) = boot.project {
        focus.set(Some(id));
    }
    route.set(boot.route);
}

/// Where Back goes when no picker or drawer is open.
pub(crate) fn route_after_back(route: Route) -> Option<Route> {
    match route {
        Route::NotificationDetail(_) => Some(Route::Notifications),
        Route::ProjectDetail => Some(Route::Projects),
        Route::Chat => None,
        _ => Some(Route::Chat),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_and_back_routes_open_a_project_and_leave_chat_alone() {
        let list = boot_route("/projects").unwrap();
        assert_eq!(list.route, Route::Projects);
        assert!(list.project.is_none());
        let detail = boot_route("/projects/proj-trip").unwrap();
        assert_eq!(detail.route, Route::ProjectDetail);
        assert_eq!(detail.project.as_deref(), Some("proj-trip"));
        let note = boot_route("/notifications/7").unwrap();
        assert_eq!(note.route, Route::NotificationDetail(7));
        assert!(boot_route("/chat").is_none());
        assert_eq!(
            route_after_back(Route::ProjectDetail),
            Some(Route::Projects)
        );
        assert_eq!(
            route_after_back(Route::NotificationDetail(3)),
            Some(Route::Notifications)
        );
        assert_eq!(route_after_back(Route::Library), Some(Route::Chat));
        assert_eq!(route_after_back(Route::Chat), None);
    }
}
