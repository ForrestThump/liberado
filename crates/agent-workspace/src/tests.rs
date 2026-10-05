use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;

use async_trait::async_trait;
use liberado_provider::{ToolDef, ToolInvocation};
use liberado_tool_runtime::ToolRuntime;
use serde_json::json;

use crate::id::directory_name;
use crate::quota::ENTRY_COST;
use crate::sandbox_tests::append_component;
use crate::workspace::{AgentWorkspace, EntryKind, WorkspaceSettings};
use crate::{DEFAULT_CAP_BYTES, WorkspaceError, WorkspaceRuntime};

const ROOM: u64 = ENTRY_COST * 8;

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
fn an_unusable_root_is_not_offered() {
    let root = scratch();
    let file = root.path().join("not-a-dir");
    fs::write(&file, "x").unwrap();
    let blocked = WorkspaceSettings {
        root: file,
        max_bytes: 1,
    };
    assert!(!blocked.root_is_usable());

    let ready = WorkspaceSettings {
        root: root.path().join("agents"),
        max_bytes: 1,
    };
    assert!(ready.root_is_usable());
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
    let left = open(root.path(), "Agent", ROOM);
    let right = open(root.path(), "agent", ROOM);
    let nested = open(root.path(), "../outside", ROOM);

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
    let first = open(root.path(), "same", ROOM);
    first.write_text("note.txt", "hello").unwrap();
    let second = open(root.path(), "same", ROOM);
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
    let cap = ENTRY_COST + 10;
    let workspace = open(root.path(), "cap", cap);
    workspace.write_text("ok.txt", "1234567890").unwrap();

    let err = workspace.write_text("over.txt", "12345678901").unwrap_err();
    assert!(matches!(err, WorkspaceError::OverCap { cap: c, .. } if c == cap));
    assert!(!workspace.files_dir().join("over.txt").exists());

    let err = workspace.write_text("ok.txt", "12345678901").unwrap_err();
    assert!(matches!(err, WorkspaceError::OverCap { .. }));
    assert_eq!(workspace.read_text("ok.txt").unwrap(), "1234567890");
}

#[test]
fn replace_counts_the_new_size_not_the_sum() {
    let root = scratch();
    let workspace = open(root.path(), "cap", ENTRY_COST + 8);
    workspace.write_text("ok.txt", "123456").unwrap();
    workspace.write_text("ok.txt", "12345678").unwrap();
    assert_eq!(workspace.read_text("ok.txt").unwrap(), "12345678");
    assert_eq!(workspace.usage_bytes().unwrap(), ENTRY_COST + 8);
}

#[test]
fn list_reports_files_directories_and_symlinks() {
    let root = scratch();
    let workspace = open(root.path(), "boxed", ROOM);
    workspace.write_text("b.txt", "hi").unwrap();
    workspace.write_text("a/c.txt", "x").unwrap();
    let link = workspace.files_dir().join("m-link");
    create_symlink(&workspace.files_dir().join("b.txt"), &link).expect("symlink");

    let entries = workspace.list(".").unwrap();
    let described: Vec<_> = entries
        .iter()
        .map(|entry| (entry.name.as_str(), entry.kind, entry.bytes))
        .collect();
    assert_eq!(
        described,
        vec![
            ("a", EntryKind::Dir, 0),
            ("b.txt", EntryKind::File, 2),
            ("m-link", EntryKind::Symlink, 0),
        ]
    );
    let nested = workspace.list("a").unwrap();
    assert_eq!(nested.len(), 1);
    assert_eq!(nested[0].name, "c.txt");
    assert_eq!(nested[0].bytes, 1);

    let err = workspace.list("b.txt").unwrap_err();
    assert!(matches!(err, WorkspaceError::NotADirectory), "{err:?}");
    let err = workspace.list("missing").unwrap_err();
    assert!(matches!(err, WorkspaceError::NotFound), "{err:?}");

    let empty_root = scratch();
    let empty = open(empty_root.path(), "empty", 10);
    assert!(empty.list(".").unwrap().is_empty());
}

#[test]
fn delete_removes_a_tree_and_a_symlink_without_its_target() {
    let root = scratch();
    let workspace = open(root.path(), "boxed", ROOM);
    workspace.write_text("dir/sub/a.txt", "aaa").unwrap();
    workspace.write_text("keep.txt", "safe").unwrap();
    // The link sits inside the tree. Deleting the tree must unlink it, not the target.
    let link = workspace.files_dir().join("dir").join("link.txt");
    create_symlink(&workspace.files_dir().join("keep.txt"), &link).expect("symlink");

    workspace.delete("dir").unwrap();
    assert_eq!(workspace.read_text("keep.txt").unwrap(), "safe");
    assert!(workspace.read_text("dir/sub/a.txt").is_err());
    assert!(matches!(
        workspace.list("dir").unwrap_err(),
        WorkspaceError::NotFound
    ));

    let err = workspace.delete(".").unwrap_err();
    assert!(matches!(err, WorkspaceError::DeleteRoot), "{err:?}");
    let err = workspace.delete("missing.txt").unwrap_err();
    assert!(matches!(err, WorkspaceError::NotFound), "{err:?}");
}

#[test]
fn ensure_parents_covers_escape_symlink_and_file_components() {
    let root = scratch();
    let workspace = open(root.path(), "boxed", ROOM);
    let files = workspace.files_dir().to_path_buf();

    let err = crate::sandbox::ensure_parents(&files, Path::new("/")).unwrap_err();
    assert!(matches!(err, WorkspaceError::PathEscape), "{err:?}");

    let outside = files.parent().unwrap().join("note.txt");
    let err = crate::sandbox::ensure_parents(&files, &outside).unwrap_err();
    assert!(matches!(err, WorkspaceError::PathEscape), "{err:?}");

    let escaped = append_component(
        &append_component(&append_component(&files, "sub"), ".."),
        "note.txt",
    );
    let err = crate::sandbox::ensure_parents(&files, &escaped).unwrap_err();
    assert!(matches!(err, WorkspaceError::PathEscape), "{err:?}");
    assert!(!files.join("sub").exists());

    let err = crate::sandbox::ensure_parents(&files, &files).unwrap_err();
    assert!(matches!(err, WorkspaceError::PathEscape), "{err:?}");

    let link = files.join("link");
    create_symlink(&files, &link).expect("symlink");
    let err = crate::sandbox::ensure_parents(&files, &link.join("note.txt")).unwrap_err();
    assert!(matches!(err, WorkspaceError::PathEscape), "{err:?}");

    workspace.write_text("file.txt", "x").unwrap();
    let err = crate::sandbox::ensure_parents(&files, &files.join("file.txt").join("note.txt"))
        .unwrap_err();
    assert!(matches!(err, WorkspaceError::NotADirectory), "{err:?}");

    let dest = files.join("a").join("b").join("c.txt");
    crate::sandbox::ensure_parents(&files, &dest).unwrap();
    assert!(files.join("a").join("b").is_dir());
    crate::sandbox::ensure_parents(&files, &files.join("a").join("b").join("d.txt")).unwrap();
}

#[test]
fn delete_frees_room_under_the_cap() {
    let root = scratch();
    let workspace = open(root.path(), "cap", ENTRY_COST + 10);
    workspace.write_text("ok.txt", "1234567890").unwrap();
    assert!(workspace.write_text("more.txt", "x").is_err());
    workspace.delete("ok.txt").unwrap();
    workspace.write_text("more.txt", "x").unwrap();
    assert_eq!(workspace.read_text("more.txt").unwrap(), "x");
}

#[test]
fn path_escape_does_not_write_outside_the_workspace() {
    let root = scratch();
    let workspace = open(root.path(), "boxed", ROOM);
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

/// `\` is a separator on every OS. On Unix it would otherwise be a filename character, so
/// `..\outside.txt` would be created inside the workspace instead of refused.
#[test]
fn backslash_is_a_separator_on_every_platform() {
    let root = scratch();
    let workspace = open(root.path(), "boxed", ROOM);
    workspace.write_text(r"sub\note.txt", "hi").unwrap();
    let nested = workspace.files_dir().join("sub").join("note.txt");
    assert_eq!(fs::read_to_string(&nested).unwrap(), "hi");
    assert_eq!(workspace.read_text(r"sub\note.txt").unwrap(), "hi");
    assert_eq!(workspace.read_text("sub/note.txt").unwrap(), "hi");

    let err = workspace
        .write_text(r"foo\..\outside.txt", "nope")
        .unwrap_err();
    assert!(matches!(err, WorkspaceError::PathEscape), "{err:?}");
    assert!(!root.path().join("outside.txt").exists());
    assert!(!workspace.files_dir().join("outside.txt").exists());
}

#[cfg(windows)]
#[test]
fn windows_unc_and_verbatim_paths_do_not_escape() {
    let root = scratch();
    let workspace = open(root.path(), "boxed", ROOM);
    for rel in [r"\\server\share\secret.txt", r"\\?\C:\Windows\notepad.exe"] {
        let err = workspace.write_text(rel, "nope").unwrap_err();
        assert!(
            matches!(err, WorkspaceError::PathEscape),
            "{rel} returned {err:?}"
        );
    }
}

#[test]
fn a_symlink_that_leaves_the_workspace_is_not_readable() {
    let root = scratch();
    let workspace = open(root.path(), "boxed", ROOM);
    let outside = root.path().join("secret.txt");
    fs::write(&outside, "hidden").unwrap();
    let link = workspace.files_dir().join("link.txt");
    create_symlink(&outside, &link)
        .expect("symlink creation failed; this test requires that privilege");
    let err = workspace.read_text("link.txt").unwrap_err();
    assert!(
        matches!(err, WorkspaceError::PathEscape),
        "symlink read returned {err:?}"
    );
    assert_eq!(fs::read_to_string(&outside).unwrap(), "hidden");
    // Write replaces the link. It does not open the outside target.
    workspace.write_text("link.txt", "changed").unwrap();
    assert_eq!(fs::read_to_string(&outside).unwrap(), "hidden");
    assert_eq!(workspace.read_text("link.txt").unwrap(), "changed");
    assert!(
        !fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn download_tool_refuses_a_non_public_target() {
    let root = scratch();
    let workspace = open(root.path(), "dl", ENTRY_COST + 100);
    let err = crate::apply(
        &workspace,
        "workspace_download",
        &json!({"url": "http://127.0.0.1:9/hello", "path": "a.txt"}),
    )
    .unwrap_err();
    assert!(err.contains("non-public"), "{err}");
    assert!(!workspace.files_dir().join("a.txt").exists());
}

#[test]
fn download_over_the_announced_cap_writes_nothing() {
    let root = scratch();
    let workspace = open(root.path(), "cap", ENTRY_COST + 10);
    let url = http_body(b"0123456789ABCDEF", true);
    let err = workspace
        .download_url_allowing_local("blob.bin", &url)
        .unwrap_err();
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
    let workspace = open(root.path(), "cap", ENTRY_COST + 10);
    let url = http_body(b"0123456789ABCDEF", false);
    let err = workspace
        .download_url_allowing_local("blob.bin", &url)
        .unwrap_err();
    assert!(matches!(err, WorkspaceError::OverCap { .. }), "{err:?}");
    assert!(!workspace.files_dir().join("blob.bin").exists());
    assert!(!workspace.home_dir().join("incoming.bin").exists());
}

#[test]
fn download_that_fits_replaces_without_double_counting() {
    let root = scratch();
    let workspace = open(root.path(), "cap", ENTRY_COST + 10);
    workspace.write_text("blob.bin", "12345678").unwrap();
    let url = http_body(b"abcdef", true);
    let bytes = workspace
        .download_url_allowing_local("blob.bin", &url)
        .unwrap();
    assert_eq!(bytes, 6);
    assert_eq!(workspace.read_text("blob.bin").unwrap(), "abcdef");
    assert_eq!(workspace.usage_bytes().unwrap(), ENTRY_COST + 6);
}

#[test]
fn download_refuses_a_redirect_off_http() {
    let root = scratch();
    let workspace = open(root.path(), "cap", ROOM);
    let url = http_redirect("file:///etc/passwd");
    let err = workspace
        .download_url_allowing_local("a.txt", &url)
        .unwrap_err();
    assert!(matches!(err, WorkspaceError::UnsupportedUrl), "{err:?}");
    assert!(!workspace.files_dir().join("a.txt").exists());
    assert!(!workspace.home_dir().join("incoming.bin").exists());
}

#[test]
fn download_follows_an_http_redirect() {
    let root = scratch();
    let workspace = open(root.path(), "cap", ROOM);
    let dest = http_body(b"ok", true);
    let url = http_redirect(&dest);
    let bytes = workspace
        .download_url_allowing_local("a.txt", &url)
        .unwrap();
    assert_eq!(bytes, 2);
    assert_eq!(workspace.read_text("a.txt").unwrap(), "ok");
}

#[test]
fn download_rejects_non_http_urls() {
    let root = scratch();
    let workspace = open(root.path(), "cap", ROOM);
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
    let left = open(root.path(), "left", ROOM);
    let right = open(root.path(), "right", ROOM);
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
    let workspace = open(root.path(), "runtime", ROOM);
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

fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link)
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

fn http_redirect(location: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let location = location.to_string();
    std::thread::spawn(move || {
        let Ok((mut sock, _)) = listener.accept() else {
            return;
        };
        let mut buf = [0u8; 2048];
        let _ = Read::read(&mut sock, &mut buf);
        let header = format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = Write::write_all(&mut sock, header.as_bytes());
    });
    format!("http://{addr}/start")
}
