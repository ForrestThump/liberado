use liberado_common::process::std_command;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

const DEFAULT_DISTRO: &str = "Debian";
const WSL_USER_ENV: &str = "LIBERADO_DEBIAN_WSL_USER";

pub(super) fn run(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    require_clean_commit(root)?;
    if cfg!(target_os = "linux") {
        return crate::ci_cmd::crap_for_root(root);
    }
    if !cfg!(windows) {
        return Err(
            "`just crap-linux` supports Debian/Linux and Windows with Debian under WSL".into(),
        );
    }
    run_from_windows(root)
}

fn run_from_windows(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let context = windows_run_context(root)?;
    prepare_debian_workspace(
        &context.distro,
        &context.account.user,
        &context.linux_bundle,
        &context.layout.workspace,
        &context.head,
        &context.layout.temp_dir,
    )?;
    build_driver(
        &context.distro,
        &context.account,
        &context.layout.workspace,
        &context.layout.driver_target,
    )?;
    run_driver(
        &context.distro,
        &context.account,
        &context.layout.workspace,
        &context.layout.driver_target,
        &context.layout.coverage_target,
        &context.layout.temp_dir,
    )
}

struct WindowsRunContext {
    distro: String,
    account: WslAccount,
    head: String,
    linux_bundle: String,
    layout: CacheLayout,
}

fn windows_run_context(root: &Path) -> Result<WindowsRunContext, Box<dyn std::error::Error>> {
    let distro =
        std::env::var("LIBERADO_DEBIAN_WSL_DISTRO").unwrap_or_else(|_| DEFAULT_DISTRO.into());
    let account = wsl_account(&distro)?;
    let head = super::git_text(root, &["rev-parse", "HEAD"])?;
    let bundle = create_bundle(root)?;
    let linux_bundle = wsl_path(&bundle, &distro)?;
    let cache_key = cache_key(root)?;
    let layout = cache_layout(&account.home, &cache_key);
    Ok(WindowsRunContext {
        distro,
        account,
        head,
        linux_bundle,
        layout,
    })
}

#[derive(Debug, Eq, PartialEq)]
struct CacheLayout {
    workspace: String,
    driver_target: String,
    coverage_target: String,
    temp_dir: String,
}

fn cache_layout(home: &str, cache_key: &str) -> CacheLayout {
    let root = format!("{home}/.cache/liberado/crap/{cache_key}");
    CacheLayout {
        workspace: format!("{root}/workspace"),
        driver_target: format!("{root}/driver-target"),
        coverage_target: format!("{root}/coverage-target"),
        temp_dir: format!("{root}/tmp"),
    }
}

fn require_clean_commit(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let dirty = super::git_text(root, &["status", "--porcelain", "--untracked-files=normal"])?;
    if dirty.is_empty() {
        Ok(())
    } else {
        Err("`just crap-linux` validates a committed artifact; commit or remove working-tree changes first".into())
    }
}

fn create_bundle(root: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let dir = root.join(".liberado");
    fs::create_dir_all(&dir)?;
    let bundle = dir.join("crap-linux.bundle");
    if bundle.exists() {
        fs::remove_file(&bundle)?;
    }
    let status = std_command("git")
        .current_dir(root)
        .args(["bundle", "create"])
        .arg(&bundle)
        .arg("HEAD")
        .status()?;
    if status.success() {
        Ok(bundle)
    } else {
        Err("could not bundle HEAD for the Debian CRAP workspace".into())
    }
}

fn prepare_debian_workspace(
    distro: &str,
    user: &str,
    bundle: &str,
    workspace: &str,
    head: &str,
    temp_dir: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    ensure_debian_workspace(distro, user, bundle, workspace, temp_dir)?;
    refresh_debian_workspace(distro, user, bundle, workspace, head)
}

fn ensure_debian_workspace(
    distro: &str,
    user: &str,
    bundle: &str,
    workspace: &str,
    temp_dir: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let parent = workspace
        .rsplit_once('/')
        .map_or(workspace, |(parent, _)| parent);
    checked_wsl(distro, user, &["mkdir", "-p", parent, temp_dir])?;
    if !wsl_success(distro, user, &["test", "-d", &format!("{workspace}/.git")])? {
        checked_wsl(
            distro,
            user,
            &["git", "clone", "--quiet", bundle, workspace],
        )?;
    }
    Ok(())
}

fn refresh_debian_workspace(
    distro: &str,
    user: &str,
    bundle: &str,
    workspace: &str,
    head: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    checked_wsl(
        distro,
        user,
        &[
            "git",
            "-C",
            workspace,
            "fetch",
            "--quiet",
            "--force",
            bundle,
            "HEAD:refs/liberado/crap-input",
        ],
    )?;
    checked_wsl(
        distro,
        user,
        &[
            "git", "-C", workspace, "checkout", "--quiet", "--detach", "--force", head,
        ],
    )?;
    let dirty = checked_wsl(
        distro,
        user,
        &["git", "-C", workspace, "status", "--porcelain"],
    )?;
    if decode_output(&dirty.stdout).trim().is_empty() {
        Ok(())
    } else {
        Err(
            "the managed Debian CRAP workspace is dirty; remove its cache directory and retry"
                .into(),
        )
    }
}

fn build_driver(
    distro: &str,
    account: &WslAccount,
    workspace: &str,
    driver_target: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = cargo_path(&account.home);
    let driver_env = format!("CARGO_TARGET_DIR={driver_target}");
    let path_env = format!("PATH={path}");
    checked_wsl_at(
        distro,
        &account.user,
        workspace,
        &[
            "env",
            &path_env,
            &driver_env,
            "cargo",
            "build",
            "--locked",
            "--quiet",
            "-p",
            "liberado-cli",
        ],
    )?;
    Ok(())
}

fn run_driver(
    distro: &str,
    account: &WslAccount,
    workspace: &str,
    driver_target: &str,
    coverage_target: &str,
    temp_dir: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let path_env = format!("PATH={}", cargo_path(&account.home));
    let coverage_env = format!("CARGO_TARGET_DIR={coverage_target}");
    let temp_env = format!("TMPDIR={temp_dir}");
    let executable = format!("{driver_target}/debug/liberado");
    checked_wsl_at(
        distro,
        &account.user,
        workspace,
        &[
            "env",
            &path_env,
            &coverage_env,
            &temp_env,
            &executable,
            "ci",
            "crap",
        ],
    )?;
    Ok(())
}

fn cargo_path(home: &str) -> String {
    format!("{home}/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
}

#[derive(Debug, Eq, PartialEq)]
struct WslAccount {
    user: String,
    home: String,
}

fn wsl_account(distro: &str) -> Result<WslAccount, Box<dyn std::error::Error>> {
    let output = std_command("wsl.exe")
        .args(["-d", distro, "getent", "passwd"])
        .output()
        .map_err(|error| format!("could not query Debian users: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Debian WSL distribution `{distro}` is unavailable; install it or set LIBERADO_DEBIAN_WSL_DISTRO"
        )
        .into());
    }
    let passwd = decode_output(&output.stdout);
    if let Ok(requested) = std::env::var(WSL_USER_ENV) {
        return account_named(&passwd, &requested).ok_or_else(|| {
            format!("{WSL_USER_ENV} names `{requested}`, which is not a usable Debian user").into()
        });
    }
    first_non_root_account(&passwd).ok_or_else(|| {
        format!(
            "Debian WSL needs a non-root user for permission-sensitive tests; set {WSL_USER_ENV}"
        )
        .into()
    })
}

fn first_non_root_account(passwd: &str) -> Option<WslAccount> {
    passwd.lines().find_map(parse_usable_account)
}

fn account_named(passwd: &str, requested: &str) -> Option<WslAccount> {
    passwd
        .lines()
        .filter_map(parse_usable_account)
        .find(|account| account.user == requested)
}

fn parse_usable_account(line: &str) -> Option<WslAccount> {
    let fields: Vec<_> = line.split(':').collect();
    let uid = fields.get(2)?.parse::<u32>().ok()?;
    let shell = *fields.get(6)?;
    if uid < 1000 || uid == 65_534 || shell.ends_with("nologin") || shell.ends_with("false") {
        return None;
    }
    Some(WslAccount {
        user: fields.first()?.to_string(),
        home: fields.get(5)?.to_string(),
    })
}

fn cache_key(root: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let canonical = root.canonicalize()?;
    let digest = format!(
        "{:x}",
        Sha256::digest(canonical.to_string_lossy().as_bytes())
    );
    Ok(digest[..12].to_string())
}

fn wsl_path(path: &Path, distro: &str) -> Result<String, Box<dyn std::error::Error>> {
    let input = path.to_string_lossy().replace('\\', "/");
    let output = std_command("wsl.exe")
        .args(["-d", distro, "wslpath", "-a", "-u", &input])
        .output()
        .map_err(|error| format!("could not map a path into Debian WSL: {error}"))?;
    if output.status.success() {
        Ok(decode_output(&output.stdout).trim().to_string())
    } else {
        Err(format!(
            "Debian WSL could not map `{input}`: {}",
            decode_output(&output.stderr).trim()
        )
        .into())
    }
}

fn checked_wsl(
    distro: &str,
    user: &str,
    args: &[&str],
) -> Result<Output, Box<dyn std::error::Error>> {
    checked_output(
        std_command("wsl.exe")
            .args(["-d", distro, "-u", user])
            .args(args)
            .output(),
        args,
    )
}

fn checked_wsl_at(
    distro: &str,
    user: &str,
    directory: &str,
    args: &[&str],
) -> Result<Output, Box<dyn std::error::Error>> {
    checked_output(
        std_command("wsl.exe")
            .args(["-d", distro, "-u", user, "--cd", directory])
            .args(args)
            .output(),
        args,
    )
}

fn wsl_success(
    distro: &str,
    user: &str,
    args: &[&str],
) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(std_command("wsl.exe")
        .args(["-d", distro, "-u", user])
        .args(args)
        .status()?
        .success())
}

fn checked_output(
    output: std::io::Result<Output>,
    args: &[&str],
) -> Result<Output, Box<dyn std::error::Error>> {
    let output = output?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(format!(
            "Debian command `{}` failed: {}",
            args.join(" "),
            decode_output(&output.stderr).trim()
        )
        .into())
    }
}

fn decode_output(bytes: &[u8]) -> String {
    if cfg!(windows) && bytes.chunks_exact(2).any(|pair| pair[1] == 0) {
        let words = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]));
        String::from_utf16_lossy(&words.collect::<Vec<_>>())
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{WslAccount, cache_layout, cargo_path, decode_output, first_non_root_account};

    #[test]
    fn selects_a_non_root_login_for_permission_sensitive_tests() {
        let passwd = "root:x:0:0:root:/root:/bin/bash\n\
                      daemon:x:1:1::/usr/sbin:/usr/sbin/nologin\n\
                      shiloh:x:1000:1000::/home/shiloh:/bin/bash\n";
        assert_eq!(
            first_non_root_account(passwd),
            Some(WslAccount {
                user: "shiloh".into(),
                home: "/home/shiloh".into()
            })
        );
    }

    #[test]
    fn linux_cargo_path_does_not_inherit_windows_tools() {
        let path = cargo_path("/home/test");
        assert!(path.starts_with("/home/test/.cargo/bin:"));
        assert!(!path.contains("/mnt/c/"));
    }

    #[test]
    fn native_workspace_keeps_driver_and_coverage_artifacts_separate() {
        let layout = cache_layout("/home/test", "repo");
        assert!(layout.workspace.starts_with("/home/test/"));
        assert_ne!(layout.driver_target, layout.coverage_target);
        assert!(layout.driver_target.ends_with("/driver-target"));
        assert!(layout.coverage_target.ends_with("/coverage-target"));
        assert!(layout.temp_dir.ends_with("/tmp"));
    }

    #[test]
    fn linux_driver_routes_test_temp_files_to_the_cache_disk() {
        let source = include_str!("debian_crap.rs");
        let run_driver = source
            .split_once("fn run_driver(")
            .and_then(|(_, tail)| tail.split_once("fn cargo_path("))
            .map(|(body, _)| body)
            .expect("run_driver source");
        assert!(run_driver.contains("let temp_env = format!(\"TMPDIR={temp_dir}\")"));
        assert!(run_driver.contains("&temp_env"));
        assert!(source.contains("[\"mkdir\", \"-p\", parent, temp_dir]"));
    }

    #[test]
    fn windows_utf16_command_output_is_readable() {
        let bytes: Vec<_> = "Debian missing"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        if cfg!(windows) {
            assert_eq!(decode_output(&bytes), "Debian missing");
        }
    }
}
