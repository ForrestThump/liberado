use std::collections::BTreeMap;

use proptest::prelude::*;
use toml_edit::DocumentMut;

use super::SchedulePatch;
use crate::update_schedule;

const POLICY: &str = r#"
[[zones]]
zone = "tasks"
write_class = "agent_writable"

[[zones]]
zone = "decisions"
write_class = "agent_writable"

[[zones]]
zone = "finance"
write_class = "human_only"

[[grants]]
component = "agent"
capabilities = [
    { Read = { Vault = "tasks" } },
    { Write = { Vault = "tasks" } },
    { Read = { Vault = "decisions" } },
    { ExecuteMcp = "memory-mcp" },
]
"#;

fn fixed_lines() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("name", "name = \"alpha\"\n"),
        ("enabled", "enabled = true\n"),
        ("cron_expr", "cron_expr = \"0 0 8 * * * *\"\n"),
        ("goal", "goal = \"keep the goal\"\n"),
        ("job", "job = \"task-ping\"\n"),
        ("habit_text", "habit_text = \"leave this text\"\n"),
        ("deliver", "deliver = false\n"),
    ])
}

fn topology_text(extras: &BTreeMap<String, String>) -> String {
    let mut text = String::from(
        r#"# header comment stays

vault_path = "/tmp/vault" # vault inline comment

# mcp comment stays
[[mcps]]
name = "memory-mcp"
description = "store and recall memories"
consequence = "reversible"
transport = { kind = "stdio", command = "memory-mcp", args = [] }
writes_vault = false

# alpha comment stays
[[schedules]]
name = "alpha"
enabled = true
cron_expr = "0 0 8 * * * *"
goal = "keep the goal"
job = "task-ping"
habit_text = "leave this text"
deliver = false
"#,
    );
    for (key, value) in extras {
        text.push_str(&format!(
            "# keep {key}\n{key} = \"{value}\" # inline {key}\n"
        ));
    }
    text.push_str(
        r#"
[[schedules]]
name = "beta"
enabled = true
cron_expr = "0 0 20 * * * *"
goal = "beta goal"
job = "event-ping"
unknown_beta = "beta-stays"

# end comment stays
"#,
    );
    text
}

fn extra_line(key: &str, value: &str) -> String {
    format!("# keep {key}\n{key} = \"{value}\" # inline {key}\n")
}

fn write_case(text: &str, patch: &SchedulePatch) -> (String, String) {
    let _guard = crate::survivor_tests::env_lock().lock().unwrap();
    let data = tempfile::tempdir().unwrap();
    let _env = crate::survivor_tests::EnvGuard::set("LIBERADO_DATA_DIR", data.path());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("topology.toml");
    std::fs::write(&path, text).unwrap();
    std::fs::write(dir.path().join("policy.toml"), POLICY).unwrap();
    update_schedule(&path, "alpha", patch).expect("patch");
    let after = std::fs::read_to_string(&path).unwrap();
    (text.to_string(), after)
}

fn assert_untouched(
    before: &str,
    after: &str,
    extras: &BTreeMap<String, String>,
    patch: &SchedulePatch,
) {
    let head = before.split("[[schedules]]").next().unwrap();
    assert!(after.starts_with(head), "text before [[schedules]] changed");
    assert!(after.contains("# end comment stays"), "{after}");
    assert!(after.contains("unknown_beta = \"beta-stays\""), "{after}");
    assert!(after.contains("# alpha comment stays"), "{after}");
    assert!(after.contains("# mcp comment stays"), "{after}");
    for (key, line) in fixed_lines() {
        if patch.touches(key) {
            continue;
        }
        assert!(after.contains(line), "untouched key {key} changed\n{after}");
    }
    for (key, value) in extras {
        if patch.touches(key) {
            continue;
        }
        let line = extra_line(key, value);
        assert!(
            after.contains(&line),
            "untouched key {key} changed\n{after}"
        );
    }
}

#[test]
fn a_cron_patch_keeps_job_habit_deliver_and_unknown_keys() {
    let extras = BTreeMap::from([("mystery".to_string(), "kept".to_string())]);
    let before = topology_text(&extras);
    let mut patch = SchedulePatch::default();
    patch.set_string("cron_expr", "0 30 8 * * * *");
    let (_before, after) = write_case(&before, &patch);
    assert_untouched(&before, &after, &extras, &patch);
    assert!(after.contains("cron_expr = \"0 30 8 * * * *\""));
    assert!(!after.contains("cron_expr = \"0 0 8 * * * *\""));
    let doc = after.parse::<DocumentMut>().unwrap();
    let alpha = schedule_named(&doc, "alpha");
    assert_eq!(
        alpha.get("job").and_then(|item| item.as_str()),
        Some("task-ping")
    );
    assert_eq!(
        alpha.get("habit_text").and_then(|item| item.as_str()),
        Some("leave this text")
    );
    assert_eq!(
        alpha.get("deliver").and_then(|item| item.as_bool()),
        Some(false)
    );
    assert_eq!(
        alpha.get("mystery").and_then(|item| item.as_str()),
        Some("kept")
    );
}

#[test]
fn clearing_job_without_remove_leaves_the_file() {
    let before = topology_text(&BTreeMap::new());
    let _guard = crate::survivor_tests::env_lock().lock().unwrap();
    let data = tempfile::tempdir().unwrap();
    let _env = crate::survivor_tests::EnvGuard::set("LIBERADO_DATA_DIR", data.path());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("topology.toml");
    std::fs::write(&path, &before).unwrap();
    std::fs::write(dir.path().join("policy.toml"), POLICY).unwrap();
    let mut patch = SchedulePatch::default();
    patch.clear_key("job");
    let err = update_schedule(&path, "alpha", &patch).unwrap_err();
    assert!(err.to_string().contains("refusing to remove job"), "{err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[test]
fn an_empty_goal_on_a_job_entry_is_refused() {
    let before = topology_text(&BTreeMap::new());
    let _guard = crate::survivor_tests::env_lock().lock().unwrap();
    let data = tempfile::tempdir().unwrap();
    let _env = crate::survivor_tests::EnvGuard::set("LIBERADO_DATA_DIR", data.path());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("topology.toml");
    std::fs::write(&path, &before).unwrap();
    std::fs::write(dir.path().join("policy.toml"), POLICY).unwrap();
    let mut patch = SchedulePatch::default();
    patch.set_string("goal", "");
    let err = update_schedule(&path, "alpha", &patch).unwrap_err();
    assert!(
        err.to_string()
            .contains("refusing goal = \"\" on a job entry"),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[test]
fn json_null_does_not_remove_deliver_unless_remove_names_it() {
    let err = SchedulePatch::from_tool_args(&serde_json::json!({
        "name": "alpha",
        "deliver": null
    }))
    .unwrap_err();
    assert!(err.contains("refusing to remove deliver"), "{err}");

    let patch = SchedulePatch::from_tool_args(&serde_json::json!({
        "name": "alpha",
        "remove": ["deliver"]
    }))
    .unwrap();
    let before = topology_text(&BTreeMap::new());
    let (_before, after) = write_case(&before, &patch);
    assert!(!after.contains("deliver = false"));
    assert!(after.contains("job = \"task-ping\""));
    assert!(after.contains("habit_text = \"leave this text\""));
}

#[test]
fn tool_args_patch_only_the_named_goal() {
    let patch = SchedulePatch::from_tool_args(&serde_json::json!({
        "name": "alpha",
        "goal": "still a job"
    }))
    .unwrap();
    let extras = BTreeMap::from([("mystery".to_string(), "kept".to_string())]);
    let before = topology_text(&extras);
    let (_before, after) = write_case(&before, &patch);
    assert_untouched(&before, &after, &extras, &patch);
    assert!(after.contains("goal = \"still a job\""));
}

fn schedule_named<'a>(doc: &'a DocumentMut, name: &str) -> &'a toml_edit::Table {
    doc.get("schedules")
        .and_then(|item| item.as_array_of_tables())
        .and_then(|array| {
            array
                .iter()
                .find(|table| table.get("name").and_then(|item| item.as_str()) == Some(name))
        })
        .unwrap_or_else(|| panic!("missing schedule {name}"))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn untouched_keys_stay_byte_identical(
        extras in prop::collection::btree_map("[a-z]{3,6}", "[a-z]{1,8}", 0..4),
        patch_enabled in prop::option::of(any::<bool>()),
        patch_turns in prop::option::of(1i64..20),
        patch_goal in prop::option::of("[a-z]{1,12}"),
    ) {
        let extras: BTreeMap<String, String> = extras
            .into_iter()
            .map(|(key, value)| (format!("x{key}"), value))
            .collect();
        let mut patch = SchedulePatch::default();
        if let Some(enabled) = patch_enabled {
            patch.set_bool("enabled", enabled);
        }
        if let Some(turns) = patch_turns {
            patch.set_i64("max_turns", turns);
        }
        if let Some(goal) = patch_goal {
            patch.set_string("goal", &goal);
        }
        let before = topology_text(&extras);
        let (_stored, after) = write_case(&before, &patch);
        assert_untouched(&before, &after, &extras, &patch);
    }
}
