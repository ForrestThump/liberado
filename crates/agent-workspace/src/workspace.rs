//! One agent's private directory and the file operations on it.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use fs4::fs_std::FileExt;

use crate::entries;
use crate::error::WorkspaceError;
use crate::id::directory_name;
use crate::quota::{self, admit};
use crate::sandbox;

pub use crate::entries::{DirEntry, EntryKind};

/// Directory name under the data dir when no root override is set.
pub const DEFAULT_DIR_NAME: &str = "agent-workspaces";

/// Default per-agent cap: 1 GiB (1024^3 bytes).
pub const DEFAULT_CAP_BYTES: u64 = 1024 * 1024 * 1024;

/// A single `workspace_read` returns at most this many bytes.
pub const MAX_READ_BYTES: u64 = 1024 * 1024;

const IDENTITY_FILE: &str = "agent-id";
const LOCK_FILE: &str = "write.lock";
const INCOMING_FILE: &str = "incoming.bin";
const FILES_DIR: &str = "files";

/// Where agent directories live, and the byte cap each one enforces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSettings {
    pub root: PathBuf,
    pub max_bytes: u64,
}

impl WorkspaceSettings {
    /// `root_override` empty means `<data_dir>/agent-workspaces`. The cap is the value passed in.
    /// A configured cap is not replaced with [`DEFAULT_CAP_BYTES`].
    pub fn resolve(max_bytes: u64, root_override: &str, data_dir: &Path) -> Self {
        let trimmed = root_override.trim();
        let root = if trimmed.is_empty() {
            data_dir.join(DEFAULT_DIR_NAME)
        } else {
            PathBuf::from(trimmed)
        };
        Self { root, max_bytes }
    }

    /// True when [`AgentWorkspace::open`] can create an agent directory here.
    ///
    /// A false result means every session open fails and the file tools are withheld.
    /// The boot log uses this so it does not name tools the runtime will not offer.
    pub fn root_is_usable(&self) -> bool {
        if fs::create_dir_all(&self.root).is_err() {
            return false;
        }
        match fs::canonicalize(&self.root) {
            Ok(path) => path.is_dir(),
            Err(_) => false,
        }
    }
}

/// One agent's private files. Two different agent ids never share this directory.
///
/// There is no shared scratch directory. Another agent cannot read these files. Agents share
/// through a channel or a local git repository.
#[derive(Debug, Clone)]
pub struct AgentWorkspace {
    agent_id: String,
    home: PathBuf,
    files: PathBuf,
    max_bytes: u64,
}

impl AgentWorkspace {
    /// Open or create the private directory for `agent_id` under `root`.
    ///
    /// The same id opens the same directory. A different id gets a different directory. If the
    /// directory exists but was bound to another id, this returns an error and does not use it.
    pub fn open(root: &Path, agent_id: &str, max_bytes: u64) -> Result<Self, WorkspaceError> {
        let name = directory_name(agent_id)?;
        fs::create_dir_all(root).map_err(WorkspaceError::io)?;
        let root = fs::canonicalize(root).map_err(WorkspaceError::io)?;
        let home = root.join(&name);
        if home.parent() != Some(root.as_path()) {
            return Err(WorkspaceError::PathEscape);
        }
        fs::create_dir_all(&home).map_err(WorkspaceError::io)?;
        bind_identity(&home, agent_id)?;
        let files = home.join(FILES_DIR);
        fs::create_dir_all(&files).map_err(WorkspaceError::io)?;
        let files = fs::canonicalize(&files).map_err(WorkspaceError::io)?;
        let home = fs::canonicalize(&home).map_err(WorkspaceError::io)?;
        if !files.starts_with(&home) || !home.starts_with(&root) {
            return Err(WorkspaceError::PathEscape);
        }
        Ok(Self {
            agent_id: agent_id.to_string(),
            home,
            files,
            max_bytes,
        })
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn home_dir(&self) -> &Path {
        &self.home
    }

    pub fn files_dir(&self) -> &Path {
        &self.files
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    pub fn usage_bytes(&self) -> Result<u64, WorkspaceError> {
        let _guard = self.lock()?;
        quota::usage(&self.files)
    }

    pub fn list(&self, rel: &str) -> Result<Vec<DirEntry>, WorkspaceError> {
        let _guard = self.lock()?;
        let path = sandbox::resolve_following(&self.files, rel)?;
        entries::list_directory(&path)
    }

    pub fn read_text(&self, rel: &str) -> Result<String, WorkspaceError> {
        let _guard = self.lock()?;
        let path = self.existing_file(rel)?;
        let len = quota::file_len(&path)?;
        if len > MAX_READ_BYTES {
            return Err(WorkspaceError::ReadTooLarge {
                len,
                max: MAX_READ_BYTES,
            });
        }
        let bytes = fs::read(&path).map_err(WorkspaceError::io)?;
        String::from_utf8(bytes)
            .map_err(|_| WorkspaceError::BadPath("file is not UTF-8 text".into()))
    }

    pub fn write_text(&self, rel: &str, content: &str) -> Result<u64, WorkspaceError> {
        let _guard = self.lock()?;
        let bytes = content.as_bytes();
        let dest = self.destination(rel)?;
        let old = quota::replaced_bytes(&dest)?;
        let used = quota::usage(&self.files)?;
        admit(used, old, bytes.len() as u64, self.max_bytes)?;
        sandbox::ensure_parents(&self.files, &dest)?;
        entries::write_replacing_link(&dest, bytes)?;
        Ok(bytes.len() as u64)
    }

    pub fn delete(&self, rel: &str) -> Result<(), WorkspaceError> {
        let _guard = self.lock()?;
        let path = sandbox::resolve(&self.files, rel)?;
        if path == self.files {
            return Err(WorkspaceError::DeleteRoot);
        }
        entries::remove_contained(&path)
    }

    /// Download `url` into `rel`. The byte cap applies to the bytes actually stored.
    ///
    /// Redirects stay on http or https. A `Location` with another scheme is refused.
    pub fn download_url(&self, rel: &str, url: &str) -> Result<u64, WorkspaceError> {
        let url = validate_url(url)?;
        let _guard = self.lock()?;
        let dest = self.destination(rel)?;
        let old = quota::replaced_bytes(&dest)?;
        let used = quota::usage(&self.files)?;
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::custom(redirect_policy))
            .timeout(Duration::from_secs(600))
            .build()
            .map_err(WorkspaceError::io)?;
        let response = client.get(url).send().map_err(download_error)?;
        // A 3xx that survived the client is a redirect reqwest could not turn
        // into an http(s) request. `http::Uri` drops `file:` before the policy
        // runs, so the response would otherwise look like a finished download.
        if response.status().is_redirection() {
            return Err(WorkspaceError::UnsupportedUrl);
        }
        if !response.status().is_success() {
            return Err(WorkspaceError::io(format!("HTTP {}", response.status())));
        }
        if let Some(announced) = response.content_length() {
            admit(used, old, announced, self.max_bytes)?;
        }
        self.store_reader(&dest, used, old, response)
    }

    fn store_reader(
        &self,
        dest: &Path,
        used: u64,
        old: u64,
        mut reader: impl Read,
    ) -> Result<u64, WorkspaceError> {
        let incoming_path = self.home.join(INCOMING_FILE);
        let _ = fs::remove_file(&incoming_path);
        let mut incoming = Incoming {
            path: incoming_path,
            keep: false,
        };
        let mut file = File::create(&incoming.path).map_err(WorkspaceError::io)?;
        let mut written = 0u64;
        let mut buf = [0u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buf).map_err(WorkspaceError::io)?;
            if read == 0 {
                break;
            }
            let next = written.saturating_add(read as u64);
            admit(used, old, next, self.max_bytes)?;
            file.write_all(&buf[..read]).map_err(WorkspaceError::io)?;
            written = next;
        }
        file.sync_all().map_err(WorkspaceError::io)?;
        drop(file);
        sandbox::ensure_parents(&self.files, dest)?;
        entries::replace_file(&incoming.path, dest)?;
        incoming.keep = true;
        Ok(written)
    }

    fn existing_file(&self, rel: &str) -> Result<PathBuf, WorkspaceError> {
        let path = sandbox::resolve_following(&self.files, rel)?;
        if path == self.files {
            return Err(WorkspaceError::NotAFile);
        }
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => Ok(path),
            Ok(_) => Err(WorkspaceError::NotAFile),
            Err(err) => Err(WorkspaceError::missing(err)),
        }
    }

    fn destination(&self, rel: &str) -> Result<PathBuf, WorkspaceError> {
        let path = sandbox::resolve(&self.files, rel)?;
        if path == self.files {
            return Err(WorkspaceError::BadPath("a file path is required".into()));
        }
        entries::reject_real_dir(&path)?;
        Ok(path)
    }

    fn lock(&self) -> Result<LockGuard, WorkspaceError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.home.join(LOCK_FILE))
            .map_err(WorkspaceError::io)?;
        FileExt::lock_exclusive(&file).map_err(WorkspaceError::io)?;
        Ok(LockGuard(file))
    }
}

struct LockGuard(File);

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

struct Incoming {
    path: PathBuf,
    keep: bool,
}

impl Drop for Incoming {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn bind_identity(home: &Path, agent_id: &str) -> Result<(), WorkspaceError> {
    let path = home.join(IDENTITY_FILE);
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            file.write_all(agent_id.as_bytes())
                .map_err(WorkspaceError::io)?;
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = fs::read(&path).map_err(WorkspaceError::io)?;
            if existing == agent_id.as_bytes() {
                Ok(())
            } else {
                Err(WorkspaceError::IdentityMismatch {
                    dir: home.to_path_buf(),
                })
            }
        }
        Err(err) => Err(WorkspaceError::io(err)),
    }
}

const MAX_REDIRECTS: usize = 5;

fn redirect_policy(attempt: reqwest::redirect::Attempt) -> reqwest::redirect::Action {
    if attempt.previous().len() >= MAX_REDIRECTS {
        return attempt.error(WorkspaceError::io("too many redirects"));
    }
    match validate_url(attempt.url().as_str()) {
        Ok(_) => attempt.follow(),
        Err(err) => attempt.error(err),
    }
}

fn download_error(err: reqwest::Error) -> WorkspaceError {
    let mut source = std::error::Error::source(&err);
    while let Some(inner) = source {
        if let Some(WorkspaceError::UnsupportedUrl) = inner.downcast_ref::<WorkspaceError>() {
            return WorkspaceError::UnsupportedUrl;
        }
        source = std::error::Error::source(inner);
    }
    WorkspaceError::io(err)
}

fn validate_url(url: &str) -> Result<&str, WorkspaceError> {
    let url = url.trim();
    if url.chars().any(|ch| ch.is_control() || ch.is_whitespace()) {
        return Err(WorkspaceError::UnsupportedUrl);
    }
    let scheme = url
        .split_once("://")
        .map(|(scheme, _)| scheme)
        .unwrap_or("");
    if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") {
        Ok(url)
    } else {
        Err(WorkspaceError::UnsupportedUrl)
    }
}
