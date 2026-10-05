//! Entry cost: every file, directory, and symlink counts, and cap 0 refuses writes.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;

use crate::WorkspaceError;
use crate::quota::{self, ENTRY_COST};
use crate::workspace::AgentWorkspace;

fn open(root: &Path, cap: u64) -> AgentWorkspace {
    AgentWorkspace::open(root, "quota", cap).unwrap()
}

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn download(workspace: &AgentWorkspace, rel: &str, body: &[u8]) -> Result<u64, WorkspaceError> {
    workspace.download_url_allowing_local(rel, &http_body(body))
}

#[test]
fn an_empty_file_counts_as_one_entry() {
    let root = scratch();
    let workspace = open(root.path(), ENTRY_COST);
    workspace.write_text("empty.txt", "").unwrap();
    assert_eq!(workspace.usage_bytes().unwrap(), ENTRY_COST);
}

#[test]
fn many_empty_files_hit_the_cap() {
    let root = scratch();
    let cap = ENTRY_COST * 3;
    let workspace = open(root.path(), cap);
    workspace.write_text("a.txt", "").unwrap();
    workspace.write_text("b.txt", "").unwrap();
    workspace.write_text("c.txt", "").unwrap();
    let err = workspace.write_text("d.txt", "").unwrap_err();
    assert!(matches!(err, WorkspaceError::OverCap { cap: got, .. } if got == cap));
    assert!(workspace.read_text("d.txt").is_err());
}

#[test]
fn empty_directories_count_toward_the_cap() {
    let root = scratch();
    let workspace = open(root.path(), ENTRY_COST * 3);
    let files = workspace.files_dir();
    fs::create_dir(files.join("a")).unwrap();
    fs::create_dir(files.join("a").join("b")).unwrap();
    fs::create_dir(files.join("a").join("b").join("c")).unwrap();
    assert_eq!(workspace.usage_bytes().unwrap(), ENTRY_COST * 3);
    assert!(workspace.write_text("d.txt", "").is_err());
    assert!(!files.join("d.txt").exists());
}

#[test]
fn a_nested_write_pays_for_each_new_parent() {
    let root = scratch();
    let workspace = open(root.path(), ENTRY_COST * 3);
    workspace.write_text("a/b/c.txt", "").unwrap();
    assert_eq!(workspace.usage_bytes().unwrap(), ENTRY_COST * 3);
    let err = workspace.write_text("a/b/d/e.txt", "").unwrap_err();
    assert!(matches!(err, WorkspaceError::OverCap { .. }), "{err:?}");
    assert!(!workspace.files_dir().join("a").join("b").join("d").exists());
}

#[test]
fn a_refused_nested_write_creates_no_directory() {
    let root = scratch();
    let workspace = open(root.path(), ENTRY_COST * 2);
    let err = workspace.write_text("a/b/c.txt", "").unwrap_err();
    assert!(matches!(err, WorkspaceError::OverCap { .. }), "{err:?}");
    assert!(!workspace.files_dir().join("a").exists());
}

#[test]
fn cap_zero_refuses_every_write_and_download() {
    let root = scratch();
    let workspace = open(root.path(), 0);
    let err = workspace.write_text("empty.txt", "").unwrap_err();
    assert!(matches!(
        err,
        WorkspaceError::OverCap {
            adding: ENTRY_COST,
            would_use: ENTRY_COST,
            cap: 0
        }
    ));
    assert!(workspace.write_text("x.txt", "x").is_err());
    assert!(workspace.write_text("dir/a.txt", "").is_err());
    assert!(!workspace.files_dir().join("dir").exists());
    assert!(!workspace.files_dir().join("empty.txt").exists());
    let err = download(&workspace, "down.bin", b"").unwrap_err();
    assert!(
        matches!(err, WorkspaceError::OverCap { cap: 0, .. }),
        "{err:?}"
    );
    assert!(!workspace.files_dir().join("down.bin").exists());
}

#[test]
fn an_empty_download_costs_one_entry() {
    let root = scratch();
    let workspace = open(root.path(), ENTRY_COST);
    assert_eq!(download(&workspace, "down.bin", b"").unwrap(), 0);
    assert_eq!(workspace.usage_bytes().unwrap(), ENTRY_COST);
    assert!(download(&workspace, "other.bin", b"").is_err());
}

#[test]
fn an_exact_cap_allows_one_empty_file() {
    let root = scratch();
    let workspace = open(root.path(), ENTRY_COST);
    workspace.write_text("only.txt", "").unwrap();
    assert!(workspace.write_text("next.txt", "").is_err());
    assert_eq!(workspace.usage_bytes().unwrap(), ENTRY_COST);
}

#[test]
fn deleting_an_entry_frees_its_cost() {
    let root = scratch();
    let workspace = open(root.path(), ENTRY_COST * 2);
    workspace.write_text("a.txt", "").unwrap();
    workspace.write_text("b.txt", "").unwrap();
    assert!(workspace.write_text("c.txt", "").is_err());
    workspace.delete("a.txt").unwrap();
    workspace.write_text("c.txt", "").unwrap();
    assert_eq!(workspace.usage_bytes().unwrap(), ENTRY_COST * 2);
}

#[test]
fn a_symlink_costs_one_entry_and_not_the_target_length() {
    let root = scratch();
    let workspace = open(root.path(), ENTRY_COST * 4);
    workspace.write_text("target.txt", "hello").unwrap();
    let link = workspace.files_dir().join("link.txt");
    link_to(&workspace.files_dir().join("target.txt"), &link);
    assert_eq!(workspace.usage_bytes().unwrap(), ENTRY_COST * 2 + 5);
}

#[cfg(unix)]
#[test]
fn a_directory_symlink_does_not_add_the_tree() {
    let root = scratch();
    let workspace = open(root.path(), ENTRY_COST * 8);
    workspace.write_text("tree/file.txt", "hello").unwrap();
    let alias = workspace.files_dir().join("alias");
    std::os::unix::fs::symlink(workspace.files_dir().join("tree"), &alias).unwrap();
    assert_eq!(workspace.usage_bytes().unwrap(), ENTRY_COST * 3 + 5);
}

#[test]
fn admit_write_on_a_directory_is_not_a_file() {
    let root = scratch();
    let workspace = open(root.path(), ENTRY_COST * 4);
    workspace.write_text("dir/a.txt", "x").unwrap();
    let dest = workspace.files_dir().join("dir");
    let err = quota::admit_write(workspace.files_dir(), &dest, 1, ENTRY_COST * 4).unwrap_err();
    assert!(matches!(err, WorkspaceError::NotAFile), "{err:?}");
}

#[test]
fn admit_write_rejects_a_path_outside_the_files_directory() {
    let root = scratch();
    let workspace = open(root.path(), ENTRY_COST * 4);
    let err =
        quota::admit_write(workspace.files_dir(), Path::new("/nope"), 0, ENTRY_COST).unwrap_err();
    assert!(matches!(err, WorkspaceError::PathEscape), "{err:?}");
}

fn link_to(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("symlink");
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(target, link).expect("symlink");
}

fn http_body(body: &[u8]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let body = body.to_vec();
    std::thread::spawn(move || serve_body(listener, body));
    format!("http://{addr}/blob")
}

fn serve_body(listener: TcpListener, body: Vec<u8>) {
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
}
