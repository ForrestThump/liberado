use std::path::{Path, PathBuf};

use super::{
    ScheduleWriteAction, create_schedule, delete_schedule, schedule_topology_path, update_schedule,
    utc_stamp_from_unix,
};
use crate::CronSchedule;
use crate::schedule_patch::SchedulePatch;

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

const TOPOLOGY: &str = r#"# header comment stays

vault_path = "/tmp/vault" # vault inline comment

# mcp comment stays
[[mcps]]
name = "memory-mcp"
description = "store and recall memories"
consequence = "reversible"
transport = { kind = "stdio", command = "memory-mcp", args = [] }
writes_vault = false

# morning comment stays
[[schedules]]
name = "morning"
enabled = true
cron_expr = "0 0 8 * * * *"
goal = "review the inbox"

[[schedules]]
name = "evening"
enabled = true
cron_expr = "0 0 20 * * * *"
goal = "close the day"

# end comment stays
[vault_reminders]
enabled = false
"#;

fn bare(name: &str) -> CronSchedule {
    CronSchedule {
        name: name.to_string(),
        enabled: true,
        cron_expr: "0 0 12 * * * *".into(),
        goal: String::new(),
        pool: None,
        profile: None,
        deliver: None,
        max_turns: None,
        job: None,
        habit_text: None,
        task_limit: None,
        capture_path: None,
        git_remote: None,
        git_branch: None,
        git_user_name: None,
        git_user_email: None,
        run_on_start: None,
        direct: None,
        days: Vec::new(),
        at: None,
        timezone: None,
    }
}

fn with_config(test: impl FnOnce(&Path)) {
    let _guard = crate::survivor_tests::env_lock().lock().unwrap();
    let data = tempfile::tempdir().unwrap();
    let _env = crate::survivor_tests::EnvGuard::set("LIBERADO_DATA_DIR", data.path());
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("topology.toml"), TOPOLOGY).unwrap();
    std::fs::write(dir.path().join("policy.toml"), POLICY).unwrap();
    test(dir.path());
}

fn topology(dir: &Path) -> PathBuf {
    dir.join("topology.toml")
}

fn read_topology(dir: &Path) -> String {
    std::fs::read_to_string(topology(dir)).unwrap()
}

fn backups(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy();
        if name.contains(".bak-") {
            found.push(path);
        }
    }
    found.sort();
    found
}

fn outside(text: &str) -> (String, String) {
    let start = text.find("[[schedules]]").expect("schedules array");
    let end = text.find("# end comment stays").expect("end comment");
    (text[..start].to_string(), text[end..].to_string())
}

fn assert_unchanged_outside(before: &str, after: &str) {
    let (before_head, before_tail) = outside(before);
    let (after_head, after_tail) = outside(after);
    assert_eq!(before_head, after_head, "text before [[schedules]] changed");
    assert_eq!(before_tail, after_tail, "text from the end comment changed");
}

#[test]
fn utc_stamp_formats_known_instants() {
    assert_eq!(utc_stamp_from_unix(0, 0), "19700101T000000.000000000Z");
    assert_eq!(
        utc_stamp_from_unix(946_684_800, 0),
        "20000101T000000.000000000Z"
    );
}

#[test]
fn schedule_topology_path_uses_config_dir() {
    let _guard = crate::survivor_tests::env_lock().lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _env = crate::survivor_tests::EnvGuard::set("LIBERADO_CONFIG_DIR", dir.path());
    let path = schedule_topology_path().expect("config dir is set");
    assert_eq!(path, dir.path().join("topology.toml"));
}

#[test]
fn create_schedule_inserts_one_row_and_keeps_comments() {
    with_config(|dir| {
        let before = read_topology(dir);
        let mut schedule = bare("noon");
        schedule.goal = "midday check".into();
        schedule.cron_expr = "0 0 17 * * * *".into();
        let report = create_schedule(dir, schedule).expect("create");
        let after = read_topology(dir);

        assert_eq!(report.action, ScheduleWriteAction::Created);
        assert_eq!(report.name, "noon");
        assert!(report.process_recreate_required);
        assert!(report.summary().contains("boot copy"));
        assert!(report.summary().contains("recreated"));
        assert_unchanged_outside(&before, &after);
        assert!(after.contains("name = \"noon\""));
        assert!(after.contains("midday check"));
        assert!(after.contains("name = \"morning\""));
        assert!(after.contains("# morning comment stays"));
        assert!(!dir.join("topology.toml.tmp").exists());

        let copies = backups(dir);
        assert_eq!(copies.len(), 1);
        let backup = std::fs::read_to_string(&copies[0]).unwrap();
        assert_eq!(backup, before);
        let backup_name = copies[0].file_name().unwrap().to_string_lossy();
        assert!(
            backup_name.starts_with("topology.toml.bak-"),
            "{backup_name}"
        );
        assert!(backup_name.ends_with('Z'), "{backup_name}");
        assert_eq!(report.backup_path, copies[0]);
    });
}

fn strings(pairs: &[(&str, &str)]) -> SchedulePatch {
    let mut patch = SchedulePatch::default();
    for (key, value) in pairs {
        patch.set_string(*key, value);
    }
    patch
}

#[test]
fn update_schedule_patches_only_the_named_fields() {
    with_config(|dir| {
        let before = read_topology(dir);
        let patch = strings(&[
            ("goal", "review the queue"),
            ("cron_expr", "0 30 8 * * * *"),
        ]);
        let report = update_schedule(&topology(dir), "morning", &patch).expect("update");
        let after = read_topology(dir);

        assert_eq!(report.action, ScheduleWriteAction::Updated);
        assert_eq!(report.name, "morning");
        assert!(report.process_recreate_required);
        assert_unchanged_outside(&before, &after);
        assert!(after.contains("# morning comment stays"));
        assert!(after.contains("review the queue"));
        assert!(!after.contains("review the inbox"));
        assert!(after.contains("name = \"evening\""));
        assert_eq!(backups(dir).len(), 1);
    });
}

#[test]
fn delete_schedule_removes_the_named_row() {
    with_config(|dir| {
        let before = read_topology(dir);
        let report = delete_schedule(&topology(dir), "evening").expect("delete");
        let after = read_topology(dir);

        assert_eq!(report.action, ScheduleWriteAction::Deleted);
        assert_eq!(report.name, "evening");
        assert!(report.summary().contains("recreated"));
        assert_unchanged_outside(&before, &after);
        assert!(after.contains("name = \"morning\""));
        assert!(after.contains("# morning comment stays"));
        assert!(!after.contains("name = \"evening\""));
        assert!(!after.contains("close the day"));
        assert_eq!(backups(dir).len(), 1);
    });
}

#[test]
fn validate_failure_leaves_the_original_and_keeps_the_backup() {
    with_config(|dir| {
        let before = read_topology(dir);
        let patch = strings(&[("profile", "no-such-profile")]);
        let err = update_schedule(&topology(dir), "evening", &patch).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("failed validation"), "{message}");
        assert!(message.contains("profile"), "{message}");
        assert!(message.contains("no-such-profile"), "{message}");
        assert_eq!(read_topology(dir), before);
        let copies = backups(dir);
        assert_eq!(copies.len(), 1);
        assert_eq!(std::fs::read_to_string(&copies[0]).unwrap(), before);
        assert!(!dir.join("topology.toml.tmp").exists());
    });
}

#[test]
fn missing_topology_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("topology.toml");
    let err = create_schedule(&missing, bare("noon")).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("not writable"), "{message}");
    assert!(message.contains("file does not exist"), "{message}");
    assert!(message.contains("wrote nothing"), "{message}");
    assert!(!missing.exists());
    assert!(backups(dir.path()).is_empty());
}

#[test]
fn duplicate_create_refuses_and_keeps_the_backup() {
    with_config(|dir| {
        let before = read_topology(dir);
        let err = create_schedule(dir, bare("morning")).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("cannot edit"), "{message}");
        assert!(message.contains("already exists"), "{message}");
        assert!(
            message.contains("original file was not changed"),
            "{message}"
        );
        assert_eq!(read_topology(dir), before);
        assert_eq!(backups(dir).len(), 1);
    });
}

#[test]
fn invalid_utf8_fails_read_and_keeps_the_backup() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("topology.toml");
    let bytes = b"\xff\xfe topology";
    std::fs::write(&file, bytes).unwrap();
    let err = delete_schedule(&file, "morning").unwrap_err();
    let message = err.to_string();
    assert!(message.contains("failed to read"), "{message}");
    assert!(
        message.contains("original file was not changed"),
        "{message}"
    );
    assert!(message.contains("backup kept"), "{message}");
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    let copies = backups(dir.path());
    assert_eq!(copies.len(), 1);
    assert_eq!(std::fs::read(&copies[0]).unwrap(), bytes);
}

#[test]
fn blocked_temp_file_leaves_the_original_and_keeps_the_backup() {
    with_config(|dir| {
        let before = read_topology(dir);
        std::fs::create_dir(dir.join("topology.toml.tmp")).unwrap();
        let err = delete_schedule(&topology(dir), "evening").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("failed to replace"), "{message}");
        assert!(
            message.contains("original file was not changed"),
            "{message}"
        );
        assert!(message.contains("backup kept"), "{message}");
        assert_eq!(read_topology(dir), before);
        assert_eq!(backups(dir).len(), 1);
        assert!(dir.join("topology.toml.tmp").is_dir());
    });
}

#[cfg(unix)]
#[test]
fn unreadable_topology_fails_backup_and_leaves_the_original() {
    use std::os::unix::fs::PermissionsExt;

    with_config(|dir| {
        let file = topology(dir);
        let before = read_topology(dir);
        let original = std::fs::metadata(&file).unwrap().permissions();
        let mut write_only = original.clone();
        write_only.set_mode(0o222);
        std::fs::set_permissions(&file, write_only).unwrap();
        let _restore = RestorePerms {
            path: &file,
            perms: original.clone(),
        };

        let err = delete_schedule(&file, "morning").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("failed to back up"), "{message}");
        assert!(
            message.contains("original file was not changed"),
            "{message}"
        );
        std::fs::set_permissions(&file, original).unwrap();
        assert_eq!(read_topology(dir), before);
        assert!(backups(dir).is_empty());
    });
}

#[test]
fn read_only_path_refuses_and_writes_nothing() {
    with_config(|dir| {
        let file = topology(dir);
        let before = read_topology(dir);
        let original = std::fs::metadata(&file).unwrap().permissions();
        let mut readonly = original.clone();
        readonly.set_readonly(true);
        std::fs::set_permissions(&file, readonly).unwrap();
        let _restore = RestorePerms {
            path: &file,
            perms: original,
        };

        let err = delete_schedule(&file, "morning").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("not writable"), "{message}");
        assert!(message.contains("wrote nothing"), "{message}");
        assert!(backups(dir).is_empty());
        assert!(!dir.join("topology.toml.tmp").exists());
        assert_eq!(read_topology(dir), before);
    });
}

/// Puts the mode back so the temp dir can be removed on Windows.
struct RestorePerms<'a> {
    path: &'a Path,
    perms: std::fs::Permissions,
}

impl Drop for RestorePerms<'_> {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(self.path, self.perms.clone());
    }
}
