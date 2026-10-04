use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;

use async_trait::async_trait;
use liberado_provider::{ToolDef, ToolInvocation};
use liberado_tool_runtime::ToolRuntime;
use serde_json::json;

use crate::id::directory_name;
use crate::workspace::{AgentWorkspace, WorkspaceSettings};
use crate::{DEFAULT_CAP_BYTES, WorkspaceError, WorkspaceRuntime};

fn open(root: &Path, agent_id: &str, cap: u64) -> AgentWorkspace {
    AgentWorkspace::open(root, agent_id, cap).unwrap()
}

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn default_cap_is_one_gibibyte() {
    assert_eq!(DEFAULT_CAP_BYTES, 1024 * 1024 * 1024);
}

#[test]
fn settings_keep_the_configured_cap_and_root() {
    let settings = WorkspaceSettings::resolve(42, "", Path::new("/data"));
    assert_eq!(settings.max_bytes, 42);
    assert_eq!(settings.root, Path::new("/data").join("agent-workspaces"));

    let custom = WorkspaceSettings::resolve(7, "  D:\\agents  ", Path::new("/data"));
    assert_eq!(custom.max_bytes, 7);
    assert_eq!(custom.root, Path::new("D:\\agents"));
}

#[test]
fn directory_names_are_distinct_for_distinct_ids() {
    let upper = directory_name("Agent").unwrap();
    let lower = directory_name("agent").unwrap();
    assert_ne!(upper, lower);
    assert_eq!(upper, directory_name("Agent").unwrap());
    assert_eq!(upper.len(), 64);
    assert!(upper.chars().all(|ch| ch.is_ascii_hexdigit()));
    let escaped = directory_name("../outside").unwrap();
    assert!(!escaped.contains(".."));
    assert!(!escaped.contains('/') && !escaped.contains('\\'));
    assert!(directory_name("").is_err());
    assert!(directory_name("a\0b").is_err());
}

#[test]
fn distinct_agents_get_distinct_directories_and_no_shared_scratch() {
    let root = scratch();
    let left = open(root.path(), "Agent", 100);
    let right = open(root.path(), "agent", 100);
    let nested = open(root.path(), "../outside", 100);

    assert_ne!(left.home_dir(), right.home_dir());
    assert_ne!(left.files_dir(), right.files_dir());
    let canon = root.path().canonicalize().unwrap();
    for workspace in [&left, &right, &nested] {
        assert_eq!(workspace.home_dir().parent().unwrap(), canon);
        assert!(workspace.files_dir().starts_with(workspace.home_dir()));
    }

    left.write_text("only-left.txt", "secret").unwrap();
    let names: Vec<_> = right
        .list(".")
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert!(
        !names.iter().any(|name| name == "only-left.txt"),
        "the other agent must not see the file: {names:?}"
    );
    assert!(nested.read_text("only-left.txt").is_err());

    let children: Vec<_> = fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(children.len(), 3, "root children: {children:?}");
    for name in ["scratch", "shared", "tmp", "temp"] {
        assert!(
            !root.path().join(name).exists(),
            "{name} must not be created"
        );
    }
}

#[test]
fn the_same_agent_id_reopens_the_same_directory() {
    let root = scratch();
    let first = open(root.path(), "same", 100);
    first.write_text("note.txt", "hello").unwrap();
    let second = open(root.path(), "same", 100);
    assert_eq!(first.files_dir(), second.files_dir());
    assert_eq!(second.read_text("note.txt").unwrap(), "hello");
}

#[test]
fn a_directory_bound_to_another_id_is_refused() {
    let root = scratch();
    let workspace = open(root.path(), "one", 100);
    let home = workspace.home_dir().to_path_buf();
    drop(workspace);
    fs::write(home.join("agent-id"), "someone-else").unwrap();
    let err = AgentWorkspace::open(root.path(), "one", 100).unwrap_err();
    assert!(matches!(err, WorkspaceError::IdentityMismatch { .. }));
}

#[test]
fn write_over_the_cap_leaves_the_workspace_unchanged() {
    let root = scratch();
    let workspace = open(root.path(), "cap", 10);
    workspace.write_text("ok.txt", "1234567890").unwrap();

    let err = workspace.write_text("over.txt", "12345678901").unwrap_err();
    assert!(matches!(err, WorkspaceError::OverCap { cap: 10, .. }));
    assert!(!workspace.files_dir().join("over.txt").exists());

    let err = workspace.write_text("ok.txt", "12345678901").unwrap_err();
    assert!(matches!(err, WorkspaceError::OverCap { .. }));
    assert_eq!(workspace.read_text("ok.txt").unwrap(), "1234567890");
}

#[test]
fn replace_counts_the_new_size_not_the_sum() {
    let root = scratch();
    let workspace = open(root.path(), "cap", 10);
    workspace.write_text("ok.txt", "123456").unwrap();
    workspace.write_text("ok.txt", "12345678").unwrap();
    assert_eq!(workspace.read_text("ok.txt").unwrap(), "12345678");
    assert_eq!(workspace.usage_bytes().unwrap(), 8);
}

#[test]
fn delete_frees_room_under_the_cap() {
    let root = scratch();
    let workspace = open(root.path(), "cap", 10);
    workspace.write_text("ok.txt", "1234567890").unwrap();
    assert!(workspace.write_text("more.txt", "x").is_err());
    workspace.delete("ok.txt").unwrap();
    workspace.write_text("more.txt", "x").unwrap();
    assert_eq!(workspace.read_text("more.txt").unwrap(), "x");
}

#[test]
fn path_escape_does_not_write_outside_the_workspace() {
    let root = scratch();
    let workspace = open(root.path(), "boxed", 100);
    let outside = root.path().join("outside.txt");
    let beside = workspace.home_dir().join("outside.txt");

    for rel in ["../outside.txt", "foo/../../outside.txt", "..\\outside.txt"] {
        let err = workspace.write_text(rel, "nope").unwrap_err();
        assert!(
            matches!(err, WorkspaceError::PathEscape),
            "{rel} returned {err:?}"
        );
    }
    assert!(!outside.exists());
    assert!(!beside.exists());

    let absolute = std::env::temp_dir().join("liberado-ws-escape-should-not-exist.txt");
    let _ = fs::remove_file(&absolute);
    let err = workspace
        .write_text(absolute.to_str().unwrap(), "nope")
        .unwrap_err();
    assert!(matches!(err, WorkspaceError::PathEscape));
    assert!(!absolute.exists());
    assert!(workspace.write_text("bad\0name", "x").is_err());
}

#[test]
fn a_symlink_that_leaves_the_workspace_is_not_readable() {
    let root = scratch();
    let workspace = open(root.path(), "boxed", 100);
    let outside = root.path().join("secret.txt");
    fs::write(&outside, "hidden").unwrap();
    let link = workspace.files_dir().join("link.txt");
    if !try_symlink(&outside, &link) {
        return;
    }
    let err = workspace.read_text("link.txt").unwrap_err();
    assert!(
        matches!(err, WorkspaceError::PathEscape),
        "symlink read returned {err:?}"
    );
    assert_eq!(fs::read_to_string(&outside).unwrap(), "hidden");
    assert!(workspace.write_text("link.txt", "changed").is_err());
    assert_eq!(fs::read_to_string(&outside).unwrap(), "hidden");
}

#[test]
fn download_over_the_announced_cap_writes_nothing() {
    let root = scratch();
    let workspace = open(root.path(), "cap", 10);
    let url = http_body(b"0123456789ABCDEF", true);
    let err = workspace.download_url("blob.bin", &url).unwrap_err();
    assert!(
        matches!(err, WorkspaceError::OverCap { .. }),
        "download returned {err:?}"
    );
    assert!(!workspace.files_dir().join("blob.bin").exists());
    assert!(!workspace.home_dir().join("incoming.bin").exists());
}

#[test]
fn download_without_a_length_stops_at_the_cap() {
    let root = scratch();
    let workspace = open(root.path(), "cap", 10);
    let url = http_body(b"0123456789ABCDEF", false);
    let err = workspace.download_url("blob.bin", &url).unwrap_err();
    assert!(matches!(err, WorkspaceError::OverCap { .. }), "{err:?}");
    assert!(!workspace.files_dir().join("blob.bin").exists());
    assert!(!workspace.home_dir().join("incoming.bin").exists());
}

#[test]
fn download_that_fits_replaces_without_double_counting() {
    let root = scratch();
    let workspace = open(root.path(), "cap", 10);
    workspace.write_text("blob.bin", "12345678").unwrap();
    let url = http_body(b"abcdef", true);
    let bytes = workspace.download_url("blob.bin", &url).unwrap();
    assert_eq!(bytes, 6);
    assert_eq!(workspace.read_text("blob.bin").unwrap(), "abcdef");
    assert_eq!(workspace.usage_bytes().unwrap(), 6);
}

#[test]
fn download_rejects_non_http_urls() {
    let root = scratch();
    let workspace = open(root.path(), "cap", 100);
    for url in ["file:///etc/passwd", "/etc/passwd", "javascript:alert(1)"] {
        let err = workspace.download_url("a.txt", url).unwrap_err();
        assert!(
            matches!(err, WorkspaceError::UnsupportedUrl),
            "{url} returned {err:?}"
        );
    }
    assert!(!workspace.files_dir().join("a.txt").exists());
}

#[test]
fn tool_arguments_cannot_select_another_agent() {
    let root = scratch();
    let left = open(root.path(), "left", 100);
    let right = open(root.path(), "right", 100);
    right.write_text("secret.txt", "nope").unwrap();
    let err = crate::apply(
        &left,
        "workspace_read",
        &json!({"path": "secret.txt", "agent_id": "right"}),
    )
    .unwrap_err();
    assert!(err.contains("no such path") || err.contains("NotFound") || err.contains("no such"));
    assert_eq!(right.read_text("secret.txt").unwrap(), "nope");
}

#[tokio::test]
async fn runtime_handles_workspace_tools_and_leaves_other_calls_alone() {
    let root = scratch();
    let workspace = open(root.path(), "runtime", 20);
    let runtime = WorkspaceRuntime::new(workspace, Box::new(EmptyInner));
    let names: Vec<_> = runtime
        .catalog()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert!(names.contains(&"workspace_write".to_string()));
    assert!(runtime.is_read_only("workspace_read"));
    assert!(!runtime.is_read_only("workspace_write"));
    assert!(!runtime.is_read_only("workspace_download"));

    runtime
        .invoke(&ToolInvocation::new(
            "1",
            "workspace_write",
            json!({"path": "a.txt", "content": "hi"}),
        ))
        .await
        .unwrap();
    let text = runtime
        .invoke(&ToolInvocation::new(
            "2",
            "workspace_read",
            json!({"path": "a.txt"}),
        ))
        .await
        .unwrap();
    assert_eq!(text, "hi");

    let err = runtime
        .invoke(&ToolInvocation::new("3", "other_tool", json!({})))
        .await
        .unwrap_err();
    assert!(err.contains("inner"));
}

struct EmptyInner;

#[async_trait]
impl ToolRuntime for EmptyInner {
    fn catalog(&self) -> Vec<ToolDef> {
        Vec::new()
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        Err(format!("inner {}", call.name))
    }
}

fn try_symlink(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link).is_ok()
    }
}

fn http_body(body: &[u8], with_length: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let body = body.to_vec();
    std::thread::spawn(move || {
        let Ok((mut sock, _)) = listener.accept() else {
            return;
        };
        let mut buf = [0u8; 2048];
        let _ = Read::read(&mut sock, &mut buf);
        let header = if with_length {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
        } else {
            "HTTP/1.0 200 OK\r\nConnection: close\r\n\r\n".to_string()
        };
        let _ = Write::write_all(&mut sock, header.as_bytes());
        let _ = Write::write_all(&mut sock, &body);
    });
    format!("http://{addr}/blob")
}
