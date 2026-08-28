use super::*;
use sysmap_core::vocab::Vocabulary;

fn map() -> SystemMap {
    SystemMap {
        generated_at: String::new(),
        repository_root: String::new(),
        config_dir: None,
        vocabulary: Vocabulary {
            layers: vec![],
            kinds: vec![],
        },
        nodes: vec![],
        edges: vec![
            MapEdge {
                from: "a".into(),
                to: "b".into(),
                kind: EdgeKind::Dependency,
                label: String::new(),
            },
            MapEdge {
                from: "a".into(),
                to: "c".into(),
                kind: EdgeKind::Dependency,
                label: String::new(),
            },
            MapEdge {
                from: "b".into(),
                to: "c".into(),
                kind: EdgeKind::Dependency,
                label: String::new(),
            },
            MapEdge {
                from: "c".into(),
                to: "d".into(),
                kind: EdgeKind::Dependency,
                label: String::new(),
            },
        ],
    }
}

#[test]
fn selection_scope_stops_at_requested_distance() {
    assert_eq!(
        visible_scope(&map(), Some("a"), false).unwrap(),
        BTreeSet::from(["a".into(), "b".into(), "c".into()])
    );
    assert_eq!(
        visible_scope(&map(), Some("a"), true).unwrap(),
        BTreeSet::from(["a".into(), "b".into(), "c".into(), "d".into()])
    );
}

#[test]
fn no_selection_has_no_edge_scope_filter() {
    assert!(visible_scope(&map(), None, false).is_none());
}

#[test]
fn direct_selection_shows_only_incident_edges() {
    let map = map();
    let scope = visible_scope(&map, Some("a"), false);
    assert!(edge_in_selection(
        &map.edges[0],
        Some("a"),
        false,
        scope.as_ref()
    ));
    assert!(!edge_in_selection(
        &map.edges[2],
        Some("a"),
        false,
        scope.as_ref()
    ));
    let two_hop_scope = visible_scope(&map, Some("a"), true);
    assert!(edge_in_selection(
        &map.edges[2],
        Some("a"),
        true,
        two_hop_scope.as_ref()
    ));
}

#[test]
fn second_hop_edge_in_selection_requires_both_endpoints_in_scope() {
    // The `&&` on line 56 of `edge_in_selection` checks that BOTH endpoints are in the
    // two-hop scope. The `||` mutant flips it: an edge with one endpoint in scope and one
    // out (e.g. `x -> b` where x is unreachable from `a`) would be wrongly reported as
    // selected. The existing test only checks edges where both endpoints ARE in scope,
    // so the `||` mutation passes trivially.
    let map = map();
    let two_hop_scope = visible_scope(&map, Some("a"), true).unwrap();
    let out_of_scope_edge = MapEdge {
        from: "x".into(),
        to: "b".into(),
        kind: EdgeKind::Dependency,
        label: String::new(),
    };
    assert!(
        !edge_in_selection(&out_of_scope_edge, Some("a"), true, Some(&two_hop_scope)),
        "edge with one endpoint outside the two-hop scope must not be selected"
    );
}

#[test]
fn arrowhead_points_in_edge_direction_at_every_zoom_level() {
    for zoom in [MIN_ZOOM, 1.0, MAX_ZOOM] {
        let points = arrow_points(Pos2::new(100.0, 50.0), Vec2::X, zoom);
        assert_eq!(points[0], Pos2::new(100.0, 50.0));
        assert!(points[1].x < points[0].x);
        assert!(points[2].x < points[0].x);
    }
}

#[test]
fn dependency_kind_toggles_are_independent() {
    assert!(edge_kind_visible(
        EdgeKind::Dependency,
        true,
        false,
        false,
        false
    ));
    assert!(!edge_kind_visible(
        EdgeKind::DevelopmentDependency,
        true,
        false,
        false,
        false
    ));
    assert!(edge_kind_visible(
        EdgeKind::DevelopmentDependency,
        false,
        true,
        false,
        false
    ));
    assert!(edge_kind_visible(
        EdgeKind::BuildDependency,
        false,
        false,
        true,
        false
    ));
    assert!(edge_kind_visible(
        EdgeKind::Control,
        false,
        false,
        false,
        true
    ));
    assert!(edge_kind_visible(EdgeKind::Data, false, false, false, true));
}

#[test]
fn only_dependency_fan_in_grows_a_node() {
    let map = map();
    let hub = node_world_size(&map, "c", "same label");
    let outgoing = node_world_size(&map, "a", "same label");
    assert!(hub.x > outgoing.x);
    assert!(hub.y > outgoing.y);
}

#[test]
fn label_font_fits_the_dark_inset() {
    for label in ["short", "liberado-provider-openai-compat"] {
        let size = node_world_size(&map(), "a", label);
        let font = fitted_label_font(label, size);
        let estimated_width = label.chars().count() as f32 * font * 0.58;
        assert!(estimated_width <= size.x - 24.0 + 0.01);
        assert!(font <= (size.y - 16.0) * 0.55 + 0.01);
    }
    let long = "liberado-provider-openai-compat";
    let size = node_world_size(&map(), "a", long);
    assert!(fitted_label_font(long, size) < BASE_LABEL_FONT);
}
