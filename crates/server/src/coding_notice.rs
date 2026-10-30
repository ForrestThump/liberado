//! What a daemon built without the `coding` feature says about coding config.
//!
//! Boot still succeeds. This helper decides whether one warning is warranted. It does not parse
//! `[coder]` into pack types — that parse stays behind the feature, where an invalid table is a
//! startup error.

#[cfg(any(test, not(feature = "coding")))]
use liberado_session::CODING_DOMAIN;

/// Sentence shared by the CLI stubs and the goals API when the pack is not in this binary.
#[cfg(any(test, not(feature = "coding")))]
pub(crate) const FEATURE_REQUIRED: &str = "built without the `coding` feature";

/// `Some` when this config would try to use the coding pack.
///
/// An empty `[coder]` table is the example file: the header is present and every key is a
/// comment, so the loaded table is empty and this returns `None`.
#[cfg(any(test, not(feature = "coding")))]
pub(crate) fn coding_config_notice(config: &liberado_bootstrap::Config) -> Option<String> {
    if !coding_config_is_set(config) {
        return None;
    }
    Some(format!(
        "{FEATURE_REQUIRED}; [coder], a coding session profile, an enabled project, or shepherd review will not run"
    ))
}

#[cfg(any(test, not(feature = "coding")))]
fn coding_config_is_set(config: &liberado_bootstrap::Config) -> bool {
    coder_table_is_non_empty(config.tuning.coder.as_ref())
        || config
            .topology
            .session_profiles
            .iter()
            .any(|profile| profile.domain.as_deref() == Some(CODING_DOMAIN))
        || !config.enabled_projects().is_empty()
        || config.topology.shepherd.review.enabled
}

#[cfg(any(test, not(feature = "coding")))]
fn coder_table_is_non_empty(coder: Option<&toml::Value>) -> bool {
    match coder {
        Some(toml::Value::Table(table)) => !table.is_empty(),
        Some(_) => true,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> liberado_bootstrap::Config {
        liberado_bootstrap::Config::default()
    }

    #[test]
    fn an_empty_coder_table_does_not_warn() {
        let mut config = config();
        assert!(coding_config_notice(&config).is_none());
        config.tuning.coder = Some(toml::Value::Table(toml::map::Map::new()));
        assert!(
            coding_config_notice(&config).is_none(),
            "a header with only comments loads as an empty table"
        );
    }

    #[test]
    fn a_non_empty_coder_table_names_the_feature() {
        let mut config = config();
        config.tuning.coder = Some(toml::Value::Table(toml::map::Map::from_iter([(
            "repo_map".into(),
            toml::Value::Table(toml::map::Map::new()),
        )])));
        let notice = coding_config_notice(&config).expect("non-empty [coder] warns");
        assert!(notice.contains("`coding`"), "{notice}");
    }

    #[test]
    fn an_enabled_coding_profile_names_the_feature() {
        let mut config = config();
        let profile: liberado_config::SessionProfile =
            toml::from_str("name = \"code\"\ndomain = \"coding\"\n").expect("profile");
        config.topology.session_profiles.push(profile);
        let notice = coding_config_notice(&config).expect("coding profile warns");
        assert!(notice.contains("`coding`"), "{notice}");
    }

    #[test]
    fn a_disabled_coding_profile_still_names_the_feature() {
        let mut config = config();
        let profile: liberado_config::SessionProfile =
            toml::from_str("name = \"code\"\nenabled = false\ndomain = \"coding\"\n")
                .expect("profile");
        config.topology.session_profiles.push(profile);
        let notice = coding_config_notice(&config).expect("a coding profile is set");
        assert!(notice.contains("`coding`"), "{notice}");
    }

    #[test]
    fn an_enabled_project_names_the_feature() {
        let mut config = config();
        let project: liberado_config::ProjectConfig =
            toml::from_str("name = \"lib\"\nroot = \"/tmp/lib\"\n").expect("project");
        config.topology.projects.push(project);
        let notice = coding_config_notice(&config).expect("enabled project warns");
        assert!(notice.contains("`coding`"), "{notice}");
    }

    #[test]
    fn a_disabled_project_does_not_warn() {
        let mut config = config();
        let project: liberado_config::ProjectConfig =
            toml::from_str("name = \"lib\"\nroot = \"/tmp/lib\"\nenabled = false\n")
                .expect("project");
        config.topology.projects.push(project);
        assert!(coding_config_notice(&config).is_none());
    }

    #[test]
    fn shepherd_review_enabled_names_the_feature() {
        let mut config = config();
        config.topology.shepherd.review.enabled = true;
        let notice = coding_config_notice(&config).expect("shepherd review warns");
        assert!(notice.contains("`coding`"), "{notice}");
    }
}
