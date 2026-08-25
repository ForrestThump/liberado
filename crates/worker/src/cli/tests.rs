use super::{Args, EnvLookup, parse_args};
use std::path::PathBuf;

/// Fixture environment: a map, so tests never touch the process env.
fn env_of(pairs: &[(&str, &str)]) -> impl EnvLookup {
    struct Fixed(Vec<(String, String)>);
    impl EnvLookup for Fixed {
        fn var(&self, name: &str) -> Option<String> {
            self.0
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }
    Fixed(
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
}

fn argv(items: &[&str]) -> impl Iterator<Item = String> {
    items
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .into_iter()
}

fn defaults(pairs: &[(&str, &str)]) -> Args {
    parse_args(argv(&[]), &env_of(pairs)).expect("defaults parse")
}

#[test]
fn env_defaults_apply_when_no_flags_are_given() {
    let args = defaults(&[("LIBERADO_WORKER_TOKEN", "tok")]);
    assert_eq!(args.bind, "127.0.0.1:7780");
    assert_eq!(args.data_dir, PathBuf::from(".liberado"));
    assert_eq!(args.config_dir, None);
    assert_eq!(args.token, "tok");
    assert_eq!(args.max_concurrent, 2);
    assert_eq!(args.forge_token, "");
}

#[test]
fn every_flag_overrides_its_env_default() {
    let args = parse_args(
        argv(&[
            "--bind",
            "0.0.0.0:9000",
            "--data-dir",
            "/data",
            "--config-dir",
            "/cfg",
            "--model",
            "test-model",
            "--forge-url",
            "http://forge:3000",
            "--clone-base-url",
            "http://git.internal",
            "--token-env",
            "MY_TOKEN",
            "--max-concurrent",
            "4",
        ]),
        &env_of(&[("MY_TOKEN", "abc")]),
    )
    .expect("parses");
    assert_eq!(args.bind, "0.0.0.0:9000");
    assert_eq!(args.data_dir, PathBuf::from("/data"));
    assert_eq!(args.config_dir, Some(PathBuf::from("/cfg")));
    assert_eq!(args.model.as_deref(), Some("test-model"));
    assert_eq!(args.forge_url.as_deref(), Some("http://forge:3000"));
    assert_eq!(args.clone_base_url.as_deref(), Some("http://git.internal"));
    assert_eq!(args.token, "abc");
    assert_eq!(args.max_concurrent, 4);
}

#[test]
fn forge_token_comes_from_the_env_var_the_flag_names() {
    let args = parse_args(
        argv(&["--forge-token-env", "FORGE_T", "--token-env", "W_T"]),
        &env_of(&[("FORGE_T", "ft"), ("W_T", "wt")]),
    )
    .expect("parses");
    assert_eq!(args.forge_token, "ft");
    assert_eq!(args.token, "wt");

    // A missing named variable is a hard error, not an empty token.
    let error = parse_args(argv(&["--forge-token-env", "ABSENT"]), &env_of(&[]))
        .expect_err("absent var must fail");
    assert!(error.contains("ABSENT is not set"), "{error}");
}

#[test]
fn worker_token_env_var_is_the_fallback_token_source() {
    let args = defaults(&[("LIBERADO_WORKER_TOKEN", "fallback")]);
    assert_eq!(args.token, "fallback");
}

#[test]
fn a_missing_token_is_a_usage_error_naming_the_variable() {
    let error = parse_args(argv(&[]), &env_of(&[])).expect_err("no token must fail");
    assert!(error.contains("usage"), "{error}");
    assert!(error.contains("LIBERADO_WORKER_TOKEN"), "{error}");
}

#[test]
fn unknown_and_dangling_arguments_are_usage_errors() {
    let error = parse_args(
        argv(&["--mystery"]),
        &env_of(&[("LIBERADO_WORKER_TOKEN", "t")]),
    )
    .expect_err("unknown flag");
    assert!(error.contains("unknown argument: --mystery"), "{error}");

    let error = parse_args(
        argv(&["--bind"]),
        &env_of(&[("LIBERADO_WORKER_TOKEN", "t")]),
    )
    .expect_err("dangling value");
    assert!(error.contains("--bind needs a value"), "{error}");

    let error = parse_args(
        argv(&["--max-concurrent", "many"]),
        &env_of(&[("LIBERADO_WORKER_TOKEN", "t")]),
    )
    .expect_err("non-numeric concurrency");
    assert!(error.contains("--max-concurrent wants a number"), "{error}");
}
