//! Managed Cargo target directories: share what is safe, isolate what is not.
//!
//! Ordinary coding worktrees from one source root may reuse one `CARGO_TARGET_DIR`. Cargo
//! fingerprints registry crates by name and version, so a docs-only or otherwise unchanged
//! tree can reuse a warm cache instead of growing another multi-gigabyte `target/`.
//!
//! Coverage (`llvm-cov` / CRAP), mutation testing, and C3 comparison harnesses stay isolated.
//! Those jobs change profile, instrumentation, or source-root identity. Sharing them with the
//! ordinary cache evicts useful artifacts or, worse, reuses a binary from the wrong checkout.
//!
//! This module chooses paths and coordinates leases. It does not spawn Cargo.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use liberado_coder_core::WorkspaceBuildConfig;

/// Why a build may or may not share a cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetClass {
    /// Default `dev` profile, no coverage or mutation instrumentation.
    Ordinary,
    /// `llvm-cov` / CRAP coverage objects contaminate a normal cache.
    Coverage,
    /// cargo-mutants replaces crate sources; it must not evict `target/debug`.
    Mutation,
    /// C3 / harness comparison: one cache per pinned worktree, never across roots.
    Comparison,
}

impl TargetClass {
    /// Directory segment used under a managed pool.
    pub fn slug(self) -> &'static str {
        match self {
            Self::Ordinary => "ordinary",
            Self::Coverage => "coverage",
            Self::Mutation => "mutation",
            Self::Comparison => "comparison",
        }
    }

    /// Only ordinary builds may attach to a shared cache.
    pub fn may_share(self) -> bool {
        matches!(self, Self::Ordinary)
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "ordinary" => Some(Self::Ordinary),
            "coverage" => Some(Self::Coverage),
            "mutation" => Some(Self::Mutation),
            "comparison" => Some(Self::Comparison),
            _ => None,
        }
    }
}

/// How the chosen directory is meant to be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Shared,
    Isolated,
    WorktreeLocal,
}

/// A chosen target directory plus the lease that keeps its class honest.
#[derive(Debug)]
pub struct TargetLease {
    allocation: TargetAllocation,
    lock_path: Option<PathBuf>,
    reclaim_on_drop: bool,
}

impl TargetLease {
    pub fn path(&self) -> &Path {
        &self.allocation.path
    }

    pub fn kind(&self) -> TargetKind {
        self.allocation.kind
    }

    pub fn class(&self) -> TargetClass {
        self.allocation.class
    }

    pub fn allocation(&self) -> &TargetAllocation {
        &self.allocation
    }
}

impl Drop for TargetLease {
    fn drop(&mut self) {
        if let Some(lock) = &self.lock_path {
            let _ = fs::remove_file(lock);
        }
        if self.reclaim_on_drop && self.allocation.kind == TargetKind::Isolated {
            let _ = fs::remove_dir_all(&self.allocation.path);
        }
    }
}

/// Path decision without the live lock handle. Safe to clone into env maps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetAllocation {
    pub path: PathBuf,
    pub kind: TargetKind,
    pub class: TargetClass,
}

/// Inputs for one allocation.
#[derive(Debug, Clone)]
pub struct TargetRequest<'a> {
    pub source_root: &'a Path,
    pub class: TargetClass,
    /// Distinguishes isolated jobs. Required when `class` cannot share.
    pub job_id: Option<&'a str>,
    /// Delete the isolated directory when the lease drops.
    pub reclaim_on_drop: bool,
}

/// A filesystem pool that owns shared and isolated target trees.
#[derive(Debug, Clone)]
pub struct TargetPool {
    root: PathBuf,
}

impl TargetPool {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Pool used when `[coder.workspace].managed_target_root` is set.
    pub fn from_workspace_build(build: &WorkspaceBuildConfig) -> Option<Self> {
        let raw = build.managed_target_root.as_deref()?.trim();
        if raw.is_empty() {
            None
        } else {
            Some(Self::new(raw))
        }
    }

    pub fn shared_path(&self, source_root: &Path, class: TargetClass) -> PathBuf {
        self.root
            .join("shared")
            .join(source_key(source_root))
            .join(class.slug())
    }

    pub fn isolated_path(&self, class: TargetClass, job_id: &str) -> PathBuf {
        self.root
            .join("isolated")
            .join(class.slug())
            .join(sanitize_job_id(job_id))
    }

    /// Allocate a target for `request`. Ordinary jobs share; others isolate.
    pub fn allocate(&self, request: &TargetRequest<'_>) -> Result<TargetLease, TargetError> {
        if request.class.may_share() {
            return self.allocate_shared(request);
        }
        self.allocate_isolated(request)
    }

    fn allocate_shared(&self, request: &TargetRequest<'_>) -> Result<TargetLease, TargetError> {
        let path = self.shared_path(request.source_root, request.class);
        match attach_shared(&path, request.class) {
            Ok(()) => Ok(lease(path, TargetKind::Shared, request.class, None, false)),
            Err(TargetError::Busy { .. }) => self.allocate_isolated(request),
            Err(other) => Err(other),
        }
    }

    fn allocate_isolated(&self, request: &TargetRequest<'_>) -> Result<TargetLease, TargetError> {
        let job = request
            .job_id
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or(TargetError::MissingJobId {
                class: request.class,
            })?;
        let path = self.isolated_path(request.class, job);
        let lock_path = lock_path_for(&path);
        acquire_exclusive_lock(&lock_path, request.class)?;
        stamp_class(&path, request.class)?;
        Ok(lease(
            path,
            TargetKind::Isolated,
            request.class,
            Some(lock_path),
            request.reclaim_on_drop,
        ))
    }

    /// Remove isolated targets whose lock is gone or names a dead process.
    pub fn reclaim_isolated(&self, older_than: Duration) -> Result<Vec<PathBuf>, TargetError> {
        let isolated = self.root.join("isolated");
        if !isolated.is_dir() {
            return Ok(Vec::new());
        }
        let mut removed = Vec::new();
        reclaim_tree(&isolated, older_than, &mut removed)?;
        Ok(removed)
    }
}

/// Resolve the ordinary coding cache for one workspace.
///
/// Order:
/// 1. `shared_target_dir` — exact `CARGO_TARGET_DIR` (C3 pins and existing operators).
/// 2. `managed_target_root` — class-aware shared path for this source root.
/// 3. Worktree-local `target/` — the default, unchanged.
pub fn resolve_ordinary(
    build: &WorkspaceBuildConfig,
    source_root: &Path,
) -> Result<TargetAllocation, TargetError> {
    if let Some(exact) = trimmed_path(build.shared_target_dir.as_deref()) {
        return Ok(TargetAllocation {
            path: exact,
            kind: TargetKind::Shared,
            class: TargetClass::Ordinary,
        });
    }
    if let Some(pool) = TargetPool::from_workspace_build(build) {
        let lease = pool.allocate(&TargetRequest {
            source_root,
            class: TargetClass::Ordinary,
            job_id: Some("ordinary"),
            reclaim_on_drop: false,
        })?;
        let allocation = lease.allocation().clone();
        std::mem::forget(lease);
        return Ok(allocation);
    }
    Ok(TargetAllocation {
        path: source_root.join("target"),
        kind: TargetKind::WorktreeLocal,
        class: TargetClass::Ordinary,
    })
}

/// Environment overlay so warm-up and later Cargo calls use the same cache.
pub fn ordinary_target_env(
    build: &WorkspaceBuildConfig,
    source_root: &Path,
) -> Result<std::collections::BTreeMap<String, String>, TargetError> {
    let mut env = std::collections::BTreeMap::new();
    let allocation = resolve_ordinary(build, source_root)?;
    if allocation.kind != TargetKind::WorktreeLocal {
        env.insert(
            "CARGO_TARGET_DIR".to_string(),
            allocation.path.to_string_lossy().into_owned(),
        );
    }
    Ok(env)
}

/// Best-effort target for a baseline worktree: honor a live `CARGO_TARGET_DIR`,
/// then a configured ordinary cache, then `workspace/target`.
pub fn baseline_target_dir(build: Option<&WorkspaceBuildConfig>, workspace: &Path) -> PathBuf {
    if let Some(from_env) = std::env::var_os("CARGO_TARGET_DIR") {
        let path = PathBuf::from(from_env);
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    if let Some(build) = build
        && let Ok(allocation) = resolve_ordinary(build, workspace)
    {
        return allocation.path;
    }
    workspace.join("target")
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TargetError {
    #[error("isolated target class {class:?} needs a job id")]
    MissingJobId { class: TargetClass },
    #[error("target {} is class {existing:?}, refused {requested:?}", path.display())]
    Incompatible {
        path: PathBuf,
        existing: TargetClass,
        requested: TargetClass,
    },
    #[error("target {} is busy with class {class:?}", path.display())]
    Busy { path: PathBuf, class: TargetClass },
    #[error("target directory {0}: {1}")]
    Io(PathBuf, String),
}

fn lease(
    path: PathBuf,
    kind: TargetKind,
    class: TargetClass,
    lock_path: Option<PathBuf>,
    reclaim_on_drop: bool,
) -> TargetLease {
    TargetLease {
        allocation: TargetAllocation { path, kind, class },
        lock_path,
        reclaim_on_drop,
    }
}

fn attach_shared(path: &Path, class: TargetClass) -> Result<(), TargetError> {
    let lock = lock_path_for(path);
    if lock.is_file() {
        match read_lock_class(&lock) {
            Some(existing) if existing == class && class.may_share() => {}
            Some(existing) if existing != class => {
                return Err(TargetError::Incompatible {
                    path: path.to_path_buf(),
                    existing,
                    requested: class,
                });
            }
            Some(existing) => {
                if lock_holder_alive(&lock) {
                    return Err(TargetError::Busy {
                        path: path.to_path_buf(),
                        class: existing,
                    });
                }
            }
            None => {
                if lock_holder_alive(&lock) {
                    return Err(TargetError::Busy {
                        path: path.to_path_buf(),
                        class,
                    });
                }
            }
        }
    }
    stamp_class(path, class)
}

fn stamp_class(dir: &Path, class: TargetClass) -> Result<(), TargetError> {
    fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    let stamp = class_stamp(dir);
    if stamp.is_file() {
        let existing = read_class_stamp(&stamp)
            .ok_or_else(|| TargetError::Io(stamp.clone(), "class stamp is unreadable".into()))?;
        if existing != class {
            return Err(TargetError::Incompatible {
                path: dir.to_path_buf(),
                existing,
                requested: class,
            });
        }
        return Ok(());
    }
    fs::write(&stamp, class.slug()).map_err(|e| io_err(&stamp, e))
}

fn acquire_exclusive_lock(path: &Path, class: TargetClass) -> Result<(), TargetError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
    }
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => write_lock(&mut file, path, class),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if lock_holder_alive(path) {
                return Err(TargetError::Busy {
                    path: path.to_path_buf(),
                    class: read_lock_class(path).unwrap_or(class),
                });
            }
            fs::remove_file(path).map_err(|e| io_err(path, e))?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|e| io_err(path, e))?;
            write_lock(&mut file, path, class)
        }
        Err(error) => Err(io_err(path, error)),
    }
}

fn write_lock(file: &mut fs::File, path: &Path, class: TargetClass) -> Result<(), TargetError> {
    write!(file, "class={}\npid={}\n", class.slug(), std::process::id())
        .map_err(|e| io_err(path, e))?;
    file.sync_all().map_err(|e| io_err(path, e))
}

fn reclaim_tree(
    dir: &Path,
    older_than: Duration,
    removed: &mut Vec<PathBuf>,
) -> Result<(), TargetError> {
    let entries = fs::read_dir(dir).map_err(|e| io_err(dir, e))?;
    for entry in entries {
        let entry = entry.map_err(|e| io_err(dir, e))?;
        let path = entry.path();
        if path.join(".liberado-target-class").is_file() {
            if isolated_is_reclaimable(&path, older_than) {
                fs::remove_dir_all(&path).map_err(|e| io_err(&path, e))?;
                removed.push(path);
            }
            continue;
        }
        if path.is_dir() {
            reclaim_tree(&path, older_than, removed)?;
        }
    }
    Ok(())
}

fn isolated_is_reclaimable(path: &Path, older_than: Duration) -> bool {
    let lock = lock_path_for(path);
    if lock.is_file() && lock_holder_alive(&lock) {
        return false;
    }
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return true;
    };
    SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age >= older_than)
}

fn lock_holder_alive(path: &Path) -> bool {
    match read_lock_pid(path) {
        Some(0) | None => false,
        Some(pid) => process_is_alive(pid),
    }
}

fn read_lock_pid(path: &Path) -> Option<u32> {
    lock_field(path, "pid=")?.parse().ok()
}

fn read_lock_class(path: &Path) -> Option<TargetClass> {
    TargetClass::parse(&lock_field(path, "class=")?)
}

fn lock_field(path: &Path, prefix: &str) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    raw.lines()
        .find_map(|line| line.strip_prefix(prefix).map(|v| v.trim().to_string()))
}

fn read_class_stamp(path: &Path) -> Option<TargetClass> {
    TargetClass::parse(&fs::read_to_string(path).ok()?)
}

fn class_stamp(dir: &Path) -> PathBuf {
    dir.join(".liberado-target-class")
}

fn lock_path_for(dir: &Path) -> PathBuf {
    dir.join(".liberado-target.lock")
}

fn trimmed_path(raw: Option<&str>) -> Option<PathBuf> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn source_key(path: &Path) -> String {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut normalized = canon.to_string_lossy().replace('\\', "/");
    // Fold case only when this filesystem treats the two spellings as one
    // directory. Lowercasing on Linux merged `/work/Repo` and `/work/repo`.
    if case_insensitive_at(&canon) {
        normalized = normalized.to_ascii_lowercase();
    }
    format!("{:016x}", fnv1a64(normalized.as_bytes()))
}

/// True when `path` (or an ancestor) has a case-flipped name that opens the
/// same directory. Linux ext4 returns false; Windows and default macOS return true.
fn case_insensitive_at(path: &Path) -> bool {
    let mut probe = path.to_path_buf();
    loop {
        if let Some(flipped) = flipped_ascii_name(&probe) {
            let other = probe.with_file_name(flipped);
            if other.exists() && same_canonical(&probe, &other) {
                return true;
            }
        }
        match probe.parent() {
            Some(parent) if parent != probe.as_path() => probe = parent.to_path_buf(),
            _ => return false,
        }
    }
}

fn flipped_ascii_name(path: &Path) -> Option<std::ffi::OsString> {
    let name = path.file_name()?.to_string_lossy();
    let lower = name.to_ascii_lowercase();
    if lower != name {
        return Some(std::ffi::OsString::from(lower));
    }
    let upper = name.to_ascii_uppercase();
    if upper != name {
        return Some(std::ffi::OsString::from(upper));
    }
    None
}

fn same_canonical(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn sanitize_job_id(job_id: &str) -> String {
    let mut out = String::new();
    for ch in job_id.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() { "job".into() } else { out }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

fn io_err(path: &Path, error: impl std::fmt::Display) -> TargetError {
    TargetError::Io(path.to_path_buf(), error.to_string())
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    liberado_common::process::std_command("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    let output = liberado_common::process::std_command("tasklist")
        .args(["/NH", "/FI", &format!("PID eq {pid}")])
        .output();
    match output {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            text.contains(&pid.to_string())
        }
        _ => true,
    }
}

#[cfg(test)]
#[path = "cargo_targets_tests.rs"]
mod tests;
