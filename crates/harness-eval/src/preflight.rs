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

pub fn run(
    spec: &JobSpec,
    policy: &WorkerPolicy,
) -> Result<(PreflightReport, ResolvedCredential), Box<dyn Error>> {
    spec.validate()?;
    validate_policy(spec, policy)?;
    let repository = spec.repository.canonicalize()?;
    require_program("git")?;
    require_program("cargo")?;
    for sibling in ["turbovault", "turbomcp"] {
        let path = repository.join(sibling);
        if !path.is_dir() {
            return Err(format!("required path dependency is missing: {}", path.display()).into());
        }
    }
    reject_git_index_locks(&repository)?;
    let base_commit = git_capture(
        &repository,
        &["rev-parse", &format!("{}^{{commit}}", spec.base_revision)],
    )?
    .trim()
    .to_string();
    let free_bytes = available_bytes(&repository)?;
    let reserve = spec
        .limits
        .minimum_free_bytes
        .max(policy.minimum_free_bytes);
    let dependency_bytes =
        ["turbovault", "turbomcp"]
            .iter()
            .try_fold(0_u64, |total, sibling| {
                directory_size_without_caches(&repository.join(sibling))
                    .map(|size| total.saturating_add(size))
            })?;
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
    for harness in &spec.harnesses {
        match harness.id.as_str() {
            "liberado" => {
                if let Some(binary) = &harness.binary
                    && !binary.is_file()
                {
                    return Err(format!(
                        "Liberado runner override does not exist: {}",
                        binary.display()
                    )
                    .into());
                }
            }
            "pi" => {
                if let Some(binary) = &harness.binary {
                    if !binary.is_file() {
                        return Err(
                            format!("Pi binary does not exist: {}", binary.display()).into()
                        );
                    }
                } else {
                    require_program(if cfg!(windows) { "pi.cmd" } else { "pi" })?;
                }
            }
            other => return Err(format!("unsupported harness '{other}'").into()),
        }
    }
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
    Ok((
        PreflightReport {
            repository,
            base_commit,
            free_bytes,
            estimated_required_bytes,
            credential_environment: environment.clone(),
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
mod tests {
    use super::*;
    use crate::contract::*;
    use chrono::Utc;
    use liberado_common::process::std_command;
    use std::fs;
    use std::path::Path;

    fn git(repository: &Path, arguments: &[&str]) {
        let status = std_command("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .status()
            .unwrap();
        assert!(status.success(), "git {arguments:?} failed");
    }

    #[test]
    fn resolved_credentials_never_debug_the_secret() {
        let value = ResolvedCredential("top-secret".to_string());
        let debug = format!("{value:?}");
        assert!(!debug.contains("top-secret"));
        assert!(debug.contains("redacted"));
    }

    #[test]
    fn policy_binds_base_url_and_disables_binary_overrides() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().canonicalize().unwrap();
        let mut spec = JobSpec {
            version: JOB_SPEC_VERSION,
            job_id: JobId::new(),
            submitted_at: Utc::now(),
            repository: repository.clone(),
            base_revision: "deadbeef".to_string(),
            task: TaskBundle::new("task.txt", "test".to_string()).unwrap(),
            harnesses: vec![HarnessRequest {
                id: "liberado".to_string(),
                binary: Some(repository.join("runner.exe")),
            }],
            run_order: vec!["liberado".to_string()],
            model: ModelPins {
                provider: "openrouter".to_string(),
                model: "deepseek/test".to_string(),
                base_url: "https://credential-thief.invalid".to_string(),
                credential_alias: "openrouter-default".to_string(),
                thinking: "high".to_string(),
                max_turns: 1,
                sampling: SAMPLING_OMITTED.to_string(),
            },
            limits: ResourceLimits {
                compile_timeout_secs: 1,
                run_timeout_secs: 1,
                minimum_free_bytes: 1,
                verifier_repair_attempts: 0,
            },
            verifier: VerifierProfile::WorkspaceTests,
            task_aware_context: false,
            acceptance: None,
            experiment: None,
            experiment_id: String::new(),
        }
        .finalize()
        .unwrap();
        let mut policy = WorkerPolicy::for_repository(repository);
        let error = validate_policy(&spec, &policy).unwrap_err();
        assert!(error.to_string().contains("base URL"));

        spec.model.base_url = "https://openrouter.ai/api/v1".to_string();
        spec = spec.finalize().unwrap();
        let error = validate_policy(&spec, &policy).unwrap_err();
        assert!(error.to_string().contains("binary overrides"));

        policy.allow_binary_overrides = true;
        validate_policy(&spec, &policy).unwrap();
    }

    #[test]
    fn require_program_fails_for_unknown_programs() {
        let err = require_program("liberado-harness-eval-definitely-not-a-program").unwrap_err();
        assert!(
            err.to_string()
                .contains("required program is not available"),
            "{err}"
        );
    }

    #[test]
    fn preflight_flags_missing_siblings_index_locks_and_disk_space() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        fs::create_dir_all(repository.join("turbovault")).unwrap();
        fs::create_dir_all(repository.join("turbomcp")).unwrap();
        fs::write(repository.join("README.md"), "test\n").unwrap();
        git(&repository, &["init"]);
        git(&repository, &["config", "user.email", "test@example.com"]);
        git(&repository, &["config", "user.name", "Test"]);
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "-m", "base"]);
        let fake = temp.path().join("fake.exe");
        fs::write(&fake, "not executable").unwrap();

        let spec = JobSpec {
            version: JOB_SPEC_VERSION,
            job_id: JobId::new(),
            submitted_at: Utc::now(),
            repository: repository.clone(),
            base_revision: "HEAD".to_string(),
            task: TaskBundle::new("task.txt", "test task".to_string()).unwrap(),
            harnesses: vec![
                HarnessRequest {
                    id: "liberado".to_string(),
                    binary: Some(fake.clone()),
                },
                HarnessRequest {
                    id: "pi".to_string(),
                    binary: Some(fake),
                },
            ],
            run_order: default_run_order(),
            model: ModelPins {
                provider: "openrouter".to_string(),
                model: "deepseek/test".to_string(),
                base_url: "http://127.0.0.1:9".to_string(),
                credential_alias: "openrouter-default".to_string(),
                thinking: "high".to_string(),
                max_turns: 1,
                sampling: SAMPLING_OMITTED.to_string(),
            },
            limits: ResourceLimits {
                compile_timeout_secs: 1,
                run_timeout_secs: 1,
                minimum_free_bytes: 0,
                verifier_repair_attempts: 0,
            },
            verifier: VerifierProfile::WorkspaceTests,
            task_aware_context: false,
            acceptance: None,
            experiment: None,
            experiment_id: String::new(),
        }
        .finalize()
        .unwrap();

        let mut policy = WorkerPolicy::for_repository(repository.clone());
        policy.minimum_free_bytes = 0;
        policy.estimated_build_bytes_per_harness = 0;
        policy.allow_binary_overrides = true;
        policy.base_urls.insert(
            "openrouter".to_string(),
            vec!["http://127.0.0.1:9".to_string()],
        );
        policy.credential_aliases.insert(
            "openrouter-default".to_string(),
            "LIBERADO_PREFLIGHT_E2E_KEY".to_string(),
        );
        unsafe { std::env::set_var("LIBERADO_PREFLIGHT_E2E_KEY", "dummy") };

        // The happy path passes and resolves the credential without printing it.
        let (report, credential) = run(&spec, &policy).unwrap();
        assert_eq!(report.credential_environment, "LIBERADO_PREFLIGHT_E2E_KEY");
        assert_eq!(credential.expose(), "dummy");
        assert!(!format!("{credential:?}").contains("dummy"));

        // A missing path dependency is a fail-fast condition.
        fs::remove_dir_all(repository.join("turbovault")).unwrap();
        let err = run(&spec, &policy).unwrap_err();
        assert!(
            err.to_string().contains("required path dependency"),
            "{err}"
        );
        fs::create_dir(repository.join("turbovault")).unwrap();

        // A stale worktree index lock blocks the run rather than corrupting the pinned worktrees.
        let worktree_lock = repository.join(".git/worktrees/some-worktree/index.lock");
        fs::create_dir_all(worktree_lock.parent().unwrap()).unwrap();
        fs::write(&worktree_lock, "").unwrap();
        let err = run(&spec, &policy).unwrap_err();
        assert!(err.to_string().contains("index.lock"), "{err}");
        fs::remove_file(&worktree_lock).unwrap();

        // The disk reserve is enforced against the estimate.
        policy.estimated_build_bytes_per_harness = u64::MAX;
        let err = run(&spec, &policy).unwrap_err();
        assert!(err.to_string().contains("free bytes"), "{err}");

        unsafe { std::env::remove_var("LIBERADO_PREFLIGHT_E2E_KEY") };
    }

    #[test]
    fn validate_policy_rejects_disallowed_provider_model_and_base_url() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        fs::create_dir_all(&repository).unwrap();
        let spec = JobSpec {
            version: JOB_SPEC_VERSION,
            job_id: JobId::new(),
            submitted_at: Utc::now(),
            repository: repository.clone(),
            base_revision: "HEAD".to_string(),
            task: TaskBundle::new("task.txt", "test task".to_string()).unwrap(),
            harnesses: vec![HarnessRequest {
                id: "liberado".to_string(),
                binary: None,
            }],
            run_order: vec!["liberado".to_string()],
            model: ModelPins {
                provider: "openrouter".to_string(),
                model: "deepseek/test".to_string(),
                base_url: "http://127.0.0.1:9".to_string(),
                credential_alias: "openrouter-default".to_string(),
                thinking: "high".to_string(),
                max_turns: 1,
                sampling: SAMPLING_OMITTED.to_string(),
            },
            limits: ResourceLimits {
                compile_timeout_secs: 1,
                run_timeout_secs: 1,
                minimum_free_bytes: 0,
                verifier_repair_attempts: 0,
            },
            verifier: VerifierProfile::WorkspaceTests,
            task_aware_context: false,
            acceptance: None,
            experiment: None,
            experiment_id: String::new(),
        }
        .finalize()
        .unwrap();
        let mut policy = WorkerPolicy::for_repository(repository);
        policy.minimum_free_bytes = 0;
        policy.estimated_build_bytes_per_harness = 0;
        policy.allow_binary_overrides = true;
        policy.base_urls.insert(
            "openrouter".to_string(),
            vec!["http://127.0.0.1:9".to_string()],
        );
        policy.credential_aliases.insert(
            "openrouter-default".to_string(),
            "LIBERADO_PREFLIGHT_DENY_KEY".to_string(),
        );
        unsafe { std::env::set_var("LIBERADO_PREFLIGHT_DENY_KEY", "dummy") };

        let mut disallowed_provider = spec.clone();
        disallowed_provider.model.provider = "evil-provider".to_string();
        let err = run(&disallowed_provider.finalize().unwrap(), &policy).unwrap_err();
        assert!(
            err.to_string()
                .contains("provider 'evil-provider' is not allowed"),
            "{err}"
        );

        let mut disallowed_model = spec.clone();
        disallowed_model.model.model = "gpt-4o".to_string();
        let err = run(&disallowed_model.finalize().unwrap(), &policy).unwrap_err();
        assert!(
            err.to_string().contains("model 'gpt-4o' is not allowed"),
            "{err}"
        );

        let mut disallowed_url = spec.clone();
        disallowed_url.model.base_url = "https://evil.example.invalid/v1".to_string();
        let err = run(&disallowed_url.finalize().unwrap(), &policy).unwrap_err();
        assert!(err.to_string().contains("base URL"), "{err}");

        // Over the turn budget is also a policy violation.
        let mut too_many_turns = spec.clone();
        too_many_turns.model.max_turns = 401;
        let err = run(&too_many_turns.finalize().unwrap(), &policy).unwrap_err();
        assert!(err.to_string().contains("exceeds worker policy"), "{err}");

        unsafe { std::env::remove_var("LIBERADO_PREFLIGHT_DENY_KEY") };
    }
}
