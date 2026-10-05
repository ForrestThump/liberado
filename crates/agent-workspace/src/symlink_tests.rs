//! Delete and write operate on the final symlink, not on its target.

use std::fs;
use std::path::Path;

use crate::WorkspaceError;
use crate::workspace::AgentWorkspace;

fn open(root: &Path, cap: u64) -> AgentWorkspace {
    AgentWorkspace::open(root, "sym", cap).unwrap()
}

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn link_to(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("symlink");
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(target, link).expect("symlink");
}

#[test]
fn delete_unlinks_a_file_symlink_and_keeps_the_target() {
    let root = scratch();
    let workspace = open(root.path(), 100);
    workspace.write_text("target.txt", "safe").unwrap();
    let link = workspace.files_dir().join("link.txt");
    link_to(&workspace.files_dir().join("target.txt"), &link);

    assert_eq!(workspace.read_text("link.txt").unwrap(), "safe");
    workspace.delete("link.txt").unwrap();
    assert_eq!(workspace.read_text("target.txt").unwrap(), "safe");
    assert!(fs::symlink_metadata(&link).is_err());
}

#[cfg(unix)]
#[test]
fn delete_unlinks_a_directory_symlink_and_keeps_the_tree() {
    let root = scratch();
    let workspace = open(root.path(), 100);
    workspace.write_text("dir/a.txt", "tree").unwrap();
    let link = workspace.files_dir().join("link");
    std::os::unix::fs::symlink(workspace.files_dir().join("dir"), &link).unwrap();

    workspace.delete("link").unwrap();
    assert!(fs::symlink_metadata(&link).is_err());
    assert_eq!(workspace.read_text("dir/a.txt").unwrap(), "tree");
    assert!(workspace.files_dir().join("dir").join("a.txt").is_file());
}

#[test]
fn write_replaces_a_file_symlink_and_leaves_the_target() {
    let root = scratch();
    let workspace = open(root.path(), 100);
    workspace.write_text("target.txt", "original").unwrap();
    let link = workspace.files_dir().join("link.txt");
    link_to(&workspace.files_dir().join("target.txt"), &link);

    workspace.write_text("link.txt", "replaced").unwrap();
    assert_eq!(
        fs::read_to_string(workspace.files_dir().join("target.txt")).unwrap(),
        "original"
    );
    assert_eq!(workspace.read_text("link.txt").unwrap(), "replaced");
    let meta = fs::symlink_metadata(&link).unwrap();
    assert!(meta.is_file());
    assert!(!meta.file_type().is_symlink());
}

#[test]
fn delete_unlinks_a_dangling_symlink() {
    let root = scratch();
    let workspace = open(root.path(), 100);
    let link = workspace.files_dir().join("missing-link");
    link_to(&workspace.files_dir().join("no-such-target"), &link);
    workspace.delete("missing-link").unwrap();
    assert!(fs::symlink_metadata(&link).is_err());
}

#[cfg(unix)]
#[test]
fn a_parent_symlink_that_leaves_the_workspace_is_refused() {
    let root = scratch();
    let workspace = open(root.path(), 100);
    let outside = root.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let link = workspace.files_dir().join("out");
    std::os::unix::fs::symlink(&outside, &link).unwrap();

    let err = workspace.write_text("out/note.txt", "nope").unwrap_err();
    assert!(matches!(err, WorkspaceError::PathEscape), "{err:?}");
    assert!(!outside.join("note.txt").exists());
    let err = workspace.read_text("out/note.txt").unwrap_err();
    assert!(matches!(err, WorkspaceError::PathEscape), "{err:?}");
}

#[cfg(unix)]
#[test]
fn a_parent_symlink_inside_the_workspace_is_followed() {
    let root = scratch();
    let workspace = open(root.path(), 100);
    workspace.write_text("sub/keep.txt", "k").unwrap();
    let link = workspace.files_dir().join("alias");
    std::os::unix::fs::symlink(workspace.files_dir().join("sub"), &link).unwrap();

    workspace.write_text("alias/note.txt", "hi").unwrap();
    assert_eq!(workspace.read_text("sub/note.txt").unwrap(), "hi");
    assert_eq!(workspace.read_text("alias/note.txt").unwrap(), "hi");
}

#[cfg(unix)]
#[test]
fn list_follows_a_directory_symlink() {
    let root = scratch();
    let workspace = open(root.path(), 100);
    workspace.write_text("dir/a.txt", "x").unwrap();
    let link = workspace.files_dir().join("alias");
    std::os::unix::fs::symlink(workspace.files_dir().join("dir"), &link).unwrap();
    let entries = workspace.list("alias").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "a.txt");
}

#[test]
fn dot_segments_do_not_follow_a_final_symlink() {
    let root = scratch();
    let workspace = open(root.path(), 100);
    workspace.write_text("target.txt", "body").unwrap();
    let link = workspace.files_dir().join("link.txt");
    link_to(&workspace.files_dir().join("target.txt"), &link);

    workspace.delete("./link.txt").unwrap();
    assert_eq!(workspace.read_text("target.txt").unwrap(), "body");
    assert!(fs::symlink_metadata(&link).is_err());
}

#[test]
fn writing_a_directory_path_is_refused() {
    let root = scratch();
    let workspace = open(root.path(), 100);
    workspace.write_text("dir/a.txt", "x").unwrap();
    let err = workspace.write_text("dir", "nope").unwrap_err();
    assert!(matches!(err, WorkspaceError::NotAFile), "{err:?}");
    assert_eq!(workspace.read_text("dir/a.txt").unwrap(), "x");
}

#[test]
fn replace_file_leaves_a_real_directory_in_place() {
    let root = scratch();
    let workspace = open(root.path(), 100);
    let from = workspace.home_dir().join("incoming.bin");
    fs::write(&from, b"data").unwrap();
    let dir = workspace.files_dir().join("dir");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("keep.txt"), b"safe").unwrap();

    let err = crate::entries::replace_file(&from, &dir).unwrap_err();
    assert!(matches!(err, WorkspaceError::NotAFile), "{err:?}");
    assert_eq!(fs::read_to_string(dir.join("keep.txt")).unwrap(), "safe");
    assert_eq!(fs::read(from).unwrap(), b"data");
}

#[test]
fn download_replaces_a_symlink_and_leaves_the_target() {
    let root = scratch();
    let workspace = open(root.path(), 100);
    workspace.write_text("target.txt", "original").unwrap();
    let link = workspace.files_dir().join("blob.bin");
    link_to(&workspace.files_dir().join("target.txt"), &link);
    let url = http_body(b"fresh");

    let bytes = workspace.download_url("blob.bin", &url).unwrap();
    assert_eq!(bytes, 5);
    assert_eq!(
        fs::read_to_string(workspace.files_dir().join("target.txt")).unwrap(),
        "original"
    );
    assert_eq!(workspace.read_text("blob.bin").unwrap(), "fresh");
    assert!(
        !fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

fn http_body(body: &[u8]) -> String {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let body = body.to_vec();
    std::thread::spawn(move || {
        let Ok((mut sock, _)) = listener.accept() else {
            return;
        };
        let mut buf = [0u8; 2048];
        let _ = Read::read(&mut sock, &mut buf);
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = Write::write_all(&mut sock, header.as_bytes());
        let _ = Write::write_all(&mut sock, &body);
    });
    format!("http://{addr}/blob")
}
