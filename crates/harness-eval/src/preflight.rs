//! Fail-fast checks that run before a paid model request or worktree mutation.

use std::error::Error;
#[cfg(windows)]
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use liberado_common::process::std_command;

use crate::contract::{JobSpec, WORKER_CONFIG_VERSION, WorkerPolicy};

pub struct ResolvedCredential(String);

impl std::fmt::Debug for ResolvedCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ResolvedCredential([redacted])")
    }
}

impl ResolvedCredential {
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct PreflightReport {
    pub repository: PathBuf,
    pub base_commit: String,
    pub free_bytes: u64,
    pub estimated_required_bytes: u64,
    pub credential_environment: String,
}

/// Leftover local `turbovault/` / `turbomcp/` trees are optional. Cargo fetches the git+tag pins.
fn optional_sibling_bytes(repository: &Path) -> Result<u64, Box<dyn Error>> {
    ["turbovault", "turbomcp"]
        .iter()
        .try_fold(0_u64, |total, sibling| {
            let path = repository.join(sibling);
            if !path.is_dir() {
                return Ok(total);
            }
            directory_size_without_caches(&path).map(|size| total.saturating_add(size))
        })
        .map_err(|err| err.into())
}

/// Free-space vs the disk estimate (reserve + leftover-clone bytes × harness count). Returns the
/// measured free bytes and the estimate; fails when the host is too tight.
fn disk_reserve_check(
    repository: &Path,
    spec: &JobSpec,
    policy: &WorkerPolicy,
) -> Result<(u64, u64), Box<dyn Error>> {
    let free_bytes = available_bytes(repository)?;
    let reserve = spec
        .limits
        .minimum_free_bytes
        .max(policy.minimum_free_bytes);
    let dependency_bytes = optional_sibling_bytes(repository)?;
    let harness_count = spec.harnesses.len() as u64;
    let estimated_required_bytes = reserve
        .saturating_add(dependency_bytes.saturating_mul(harness_count))
        .saturating_add(
            policy
                .estimated_build_bytes_per_harness
                .saturating_mul(harness_count),
        );
    if free_bytes < estimated_required_bytes {
        return Err(format!(
            "host has {} free bytes; comparison estimate requires {} to preserve the configured reserve",
            free_bytes, estimated_required_bytes
        )
        .into());
    }
    Ok((free_bytes, estimated_required_bytes))
}

/// Every harness referenced by the job must have a reachable runner binary.
fn check_harness_binaries(spec: &JobSpec) -> Result<(), Box<dyn Error>> {
    spec.harnesses.iter().try_for_each(check_one_harness_binary)
}

fn check_one_harness_binary(
    harness: &crate::contract::HarnessRequest,
) -> Result<(), Box<dyn Error>> {
    match harness.id.as_str() {
        crate::contract::HARNESS_LIBERADO => check_liberado_override(harness),
        crate::contract::HARNESS_PI => check_external_binary(harness, "Pi", "pi"),
        crate::contract::HARNESS_HERMES => check_external_binary(harness, "Hermes", "hermes"),
        crate::contract::HARNESS_DEEPAGENTS => {
            check_external_binary(harness, "Deep Agents", "deepagents")
        }
        other => Err(format!("unsupported harness '{other}'").into()),
    }
}

fn check_liberado_override(
    harness: &crate::contract::HarnessRequest,
) -> Result<(), Box<dyn Error>> {
    match &harness.binary {
        Some(binary) if !binary.is_file() => Err(format!(
            "Liberado runner override does not exist: {}",
            binary.display()
        )
        .into()),
        _ => Ok(()),
    }
}

fn check_external_binary(
    harness: &crate::contract::HarnessRequest,
    label: &str,
    harness_id: &str,
) -> Result<(), Box<dyn Error>> {
    if let Some(binary) = &harness.binary {
        if binary.is_file() {
            Ok(())
        } else {
            Err(format!("{label} binary does not exist: {}", binary.display()).into())
        }
    } else {
        let program = crate::adapter::default_path_program(harness_id)
            .ok_or_else(|| format!("no PATH default for harness '{harness_id}'"))?;
        require_program(program)
    }
}

/// Resolve the environment the model credential must come from, then load the secret.
fn resolve_credential(
    spec: &JobSpec,
    policy: &WorkerPolicy,
) -> Result<(String, ResolvedCredential), Box<dyn Error>> {
    let environment = policy
        .credential_aliases
        .get(&spec.model.credential_alias)
        .ok_or_else(|| {
            format!(
                "worker policy does not define credential alias '{}'",
                spec.model.credential_alias
            )
        })?;
    let credential = resolve_user_credential(environment)?;
    Ok((environment.clone(), credential))
}

pub fn run(
    spec: &JobSpec,
    policy: &WorkerPolicy,
) -> Result<(PreflightReport, ResolvedCredential), Box<dyn Error>> {
    spec.validate()?;
    validate_policy(spec, policy)?;
    let repository = spec.repository.canonicalize()?;
    require_program("git")?;
    require_program("cargo")?;
    reject_git_index_locks(&repository)?;
    let base_commit = git_capture(
        &repository,
        &["rev-parse", &format!("{}^{{commit}}", spec.base_revision)],
    )?
    .trim()
    .to_string();
    let (free_bytes, estimated_required_bytes) = disk_reserve_check(&repository, spec, policy)?;
    check_harness_binaries(spec)?;
    let (credential_environment, credential) = resolve_credential(spec, policy)?;
    Ok((
        PreflightReport {
            repository,
            base_commit,
            free_bytes,
            estimated_required_bytes,
            credential_environment,
        },
        credential,
    ))
}

fn directory_size_without_caches(root: &Path) -> io::Result<u64> {
    fn visit(path: &Path) -> io::Result<u64> {
        let mut total = 0_u64;
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let name = entry.file_name();
            if entry.file_type()?.is_dir()
                && matches!(
                    name.to_str(),
                    Some(".git" | "target" | ".liberado" | ".fastembed_cache")
                )
            {
                continue;
            }
            if entry.file_type()?.is_dir() {
                total = total.saturating_add(visit(&entry.path())?);
            } else {
                total = total.saturating_add(entry.metadata()?.len());
            }
        }
        Ok(total)
    }
    visit(root)
}

fn validate_policy(spec: &JobSpec, policy: &WorkerPolicy) -> Result<(), Box<dyn Error>> {
    if policy.version != WORKER_CONFIG_VERSION {
        return Err(format!("unsupported worker policy version {}", policy.version).into());
    }
    let repository = spec.repository.canonicalize()?;
    let allowed = policy
        .repositories
        .iter()
        .filter_map(|path| path.canonicalize().ok())
        .any(|path| path == repository);
    if !allowed {
        return Err(format!(
            "repository is not allowed by worker policy: {}",
            repository.display()
        )
        .into());
    }
    if !policy
        .providers
        .iter()
        .any(|value| value == &spec.model.provider)
    {
        return Err(format!("provider '{}' is not allowed", spec.model.provider).into());
    }
    let base_url_allowed = policy
        .base_urls
        .get(&spec.model.provider)
        .is_some_and(|values| values.iter().any(|value| value == &spec.model.base_url));
    if !base_url_allowed {
        return Err(format!(
            "base URL '{}' is not allowed for provider '{}'",
            spec.model.base_url, spec.model.provider
        )
        .into());
    }
    if !policy.model_prefixes.is_empty()
        && !policy
            .model_prefixes
            .iter()
            .any(|prefix| spec.model.model.starts_with(prefix))
    {
        return Err(format!("model '{}' is not allowed", spec.model.model).into());
    }
    if spec.model.max_turns > policy.maximum_turns {
        return Err(format!(
            "max turns {} exceeds worker policy limit {}",
            spec.model.max_turns, policy.maximum_turns
        )
        .into());
    }
    if spec.limits.compile_timeout_secs > policy.maximum_compile_timeout_secs
        || spec.limits.run_timeout_secs > policy.maximum_run_timeout_secs
    {
        return Err("job timeout exceeds worker policy".into());
    }
    if !policy.allow_binary_overrides
        && spec
            .harnesses
            .iter()
            .any(|harness| harness.binary.is_some())
    {
        return Err("harness binary overrides are disabled by worker policy".into());
    }
    Ok(())
}

fn resolve_user_credential(environment: &str) -> Result<ResolvedCredential, Box<dyn Error>> {
    // The executor inherits the submitter's environment. The dispatching agent is trusted, so the
    // credential alias resolves from the process environment; there is no HKCU fallback.
    if let Some(value) = std::env::var_os(environment)
        && !value.is_empty()
    {
        return Ok(ResolvedCredential(value.to_string_lossy().into_owned()));
    }
    Err(format!(
        "credential environment '{}' is not available to the executor",
        environment
    )
    .into())
}

fn require_program(program: &str) -> Result<(), Box<dyn Error>> {
    let locator = if cfg!(windows) { "where.exe" } else { "which" };
    let output = std_command(locator).arg(program).output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("required program is not available: {program}").into())
    }
}

fn reject_git_index_locks(repository: &Path) -> Result<(), Box<dyn Error>> {
    let common = git_capture(repository, &["rev-parse", "--git-common-dir"])?;
    let mut common = PathBuf::from(common.trim());
    if common.is_relative() {
        common = repository.join(common);
    }
    let worktrees = common.join("worktrees");
    if !worktrees.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(worktrees)? {
        let lock = entry?.path().join("index.lock");
        if lock.is_file() {
            return Err(format!("Git worktree index is locked: {}", lock.display()).into());
        }
    }
    Ok(())
}

fn git_capture(path: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let output = std_command("git").arg("-C").arg(path).args(args).output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into())
    }
}

#[cfg(windows)]
fn available_bytes(path: &Path) -> Result<u64, Box<dyn Error>> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut available = 0_u64;
    let success = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if success == 0 {
        Err(std::io::Error::last_os_error().into())
    } else {
        Ok(available)
    }
}

#[cfg(not(windows))]
fn available_bytes(path: &Path) -> Result<u64, Box<dyn Error>> {
    let output = std_command("df").args(["-Pk"]).arg(path).output()?;
    if !output.status.success() {
        return Err("df failed while checking available disk space".into());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().last().ok_or("df returned no filesystem row")?;
    let blocks: u64 = line
        .split_whitespace()
        .nth_back(2)
        .ok_or("df filesystem row has no available block count")?
        .parse()?;
    Ok(blocks.saturating_mul(1024))
}

#[cfg(test)]
#[path = "preflight_tests.rs"]
mod tests;
