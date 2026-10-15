use super::{Activation, EnvSnapshot, activation, main_mxid, missing_names};

fn ready() -> EnvSnapshot {
    EnvSnapshot {
        homeserver: Some("http://tuwunel:8008/".into()),
        as_token: Some("as-secret".into()),
        hs_token: Some("hs-secret".into()),
        owner: Some("@shiloh:matrix.homelab.local".into()),
        main_room: Some("#liberado:matrix.homelab.local".into()),
        ..EnvSnapshot::default()
    }
}

#[test]
fn none_set_is_off() {
    assert!(matches!(
        activation(&EnvSnapshot::default()),
        Activation::Off
    ));
}

#[test]
fn a_partial_set_names_the_missing_variables_and_not_the_values() {
    let env = EnvSnapshot {
        homeserver: Some("http://tuwunel:8008".into()),
        owner: Some("@shiloh:matrix.homelab.local".into()),
        ..EnvSnapshot::default()
    };
    match activation(&env) {
        Activation::Partial { missing } => {
            let names = missing_names(&missing);
            assert!(names.contains("LIBERADO_MATRIX_AS_TOKEN"));
            assert!(names.contains("LIBERADO_MATRIX_HS_TOKEN"));
            assert!(names.contains("LIBERADO_MATRIX_MAIN_ROOM"));
            assert!(!names.contains("LIBERADO_MATRIX_HOMESERVER"));
            assert!(!names.contains("tuwunel"));
            assert!(!names.contains("shiloh"));
        }
        other => panic!("expected partial, got {other:?}"),
    }
}

#[test]
fn all_required_values_turn_matrix_on_with_defaults() {
    match activation(&ready()) {
        Activation::On(settings) => {
            assert_eq!(settings.homeserver, "http://tuwunel:8008");
            assert_eq!(settings.server_name, "matrix.homelab.local");
            assert_eq!(settings.main_user, "@liberado:matrix.homelab.local");
            assert_eq!(settings.as_id, "liberado");
            assert_eq!(settings.sender_localpart, "liberado_bridge");
            assert_eq!(settings.listen.to_string(), "0.0.0.0:4202");
            assert_eq!(settings.as_token, "as-secret");
            let rendered = format!("{settings:?}");
            assert!(!rendered.contains("as-secret"));
            assert!(!rendered.contains("hs-secret"));
        }
        other => panic!("expected on, got {other:?}"),
    }
}

#[test]
fn explicit_server_name_user_and_listen_are_kept() {
    let mut env = ready();
    env.server_name = Some("example.org".into());
    env.main_user = Some("liberado".into());
    env.listen = Some("127.0.0.1:9".into());
    env.as_id = Some("bridge".into());
    match activation(&env) {
        Activation::On(settings) => {
            assert_eq!(settings.server_name, "example.org");
            assert_eq!(settings.main_user, "@liberado:example.org");
            assert_eq!(settings.listen.to_string(), "127.0.0.1:9");
            assert_eq!(settings.as_id, "bridge");
        }
        other => panic!("expected on, got {other:?}"),
    }
}

#[test]
fn a_bad_listen_address_stays_off_without_echoing_secrets() {
    let mut env = ready();
    env.listen = Some("not-a-socket".into());
    match activation(&env) {
        Activation::Invalid { reason } => {
            assert!(reason.contains("LIBERADO_MATRIX_AS_LISTEN"));
            assert!(!reason.contains("as-secret"));
        }
        other => panic!("expected invalid, got {other:?}"),
    }
}

#[test]
fn a_full_main_user_id_is_not_rewritten() {
    assert_eq!(
        main_mxid("@liberado:example", "other.example"),
        "@liberado:example"
    );
}
