//! Seed a sibling coverage target with registry artifacts from the HEAD llvm-cov target.
//!
//! cargo-llvm-cov reads every profraw in its target directory, and a shared target across
//! two checkouts can reuse a workspace binary from the wrong commit. The base run therefore
//! keeps its own directory. Registry crates are keyed by their own source and the coverage
//! rustflags, so their `deps`, `.fingerprint`, and `build` outputs can be hardlinked.
//! Workspace outputs, profraw, and profdata are left behind.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub(super) fn head_llvm_cov_target(root: &Path) -> PathBuf {
    let explicit = std::env::var_os("CARGO_LLVM_COV_TARGET_DIR").map(PathBuf::from);
    let cargo_target = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from);
    resolve_llvm_cov_target(root, explicit.as_deref(), cargo_target.as_deref())
}

pub(super) fn resolve_llvm_cov_target(
    root: &Path,
    explicit: Option<&Path>,
    cargo_target: Option<&Path>,
) -> PathBuf {
    if let Some(path) = explicit {
        return absolute_under(root, path);
    }
    let base = match cargo_target {
        Some(path) => absolute_under(root, path),
        None => root.join("target"),
    };
    base.join("llvm-cov-target")
}

pub(super) fn seed_registry_artifacts(from: &Path, to: &Path, workspace: &Path) -> io::Result<u32> {
    if !from.is_dir() {
        return Ok(0);
    }
    let mut linked = 0;
    for profile in ["debug", "release"] {
        let deps = from.join(profile).join("deps");
        linked += seed_dep_dir(&deps, &to.join(profile).join("deps"), workspace)?;
        let (prints, names) = seed_fingerprint_dir(
            &from.join(profile).join(".fingerprint"),
            &to.join(profile).join(".fingerprint"),
            workspace,
        )?;
        linked += prints;
        linked += seed_named_dirs(
            &from.join(profile).join("build"),
            &to.join(profile).join("build"),
            &names,
        )?;
    }
    Ok(linked)
}

pub(super) fn scrub_profiles(dir: &Path) -> io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    scrub_tree(dir)
}

fn absolute_under(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn seed_dep_dir(from: &Path, to: &Path, workspace: &Path) -> io::Result<u32> {
    if !from.is_dir() {
        return Ok(0);
    }
    let mut linked = 0;
    for entry in fs::read_dir(from)? {
        let path = entry?.path();
        if !is_dep_info(&path) {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap_or_default();
        if !is_registry_dep_info(&text, workspace) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        linked += link_unit_files(from, to, stem)?;
    }
    Ok(linked)
}

fn seed_fingerprint_dir(
    from: &Path,
    to: &Path,
    workspace: &Path,
) -> io::Result<(u32, Vec<String>)> {
    if !from.is_dir() {
        return Ok((0, Vec::new()));
    }
    let mut linked = 0;
    let mut names = Vec::new();
    for entry in fs::read_dir(from)? {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        let text = fs::read_to_string(path.join("dep-info")).unwrap_or_default();
        if !is_registry_dep_info(&text, workspace) {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        linked += link_tree_files(&path, &to.join(name))?;
        names.push(name.to_string());
    }
    Ok((linked, names))
}

fn seed_named_dirs(from: &Path, to: &Path, names: &[String]) -> io::Result<u32> {
    if !from.is_dir() {
        return Ok(0);
    }
    let mut linked = 0;
    for name in names {
        let src = from.join(name);
        if src.is_dir() {
            linked += link_tree_files(&src, &to.join(name))?;
        }
    }
    Ok(linked)
}

fn link_unit_files(from_dir: &Path, to_dir: &Path, stem: &str) -> io::Result<u32> {
    if stem.is_empty() {
        return Ok(0);
    }
    let mut linked = 0;
    for entry in fs::read_dir(from_dir)? {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if is_profile_name(name) || !same_unit(name, stem) {
            continue;
        }
        link_file(&path, &to_dir.join(name))?;
        linked += 1;
    }
    Ok(linked)
}

fn link_tree_files(from: &Path, to: &Path) -> io::Result<u32> {
    let mut linked = 0;
    for entry in fs::read_dir(from)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if is_profile_name(name) {
            continue;
        }
        let dest = to.join(name);
        if path.is_dir() {
            linked += link_tree_files(&path, &dest)?;
        } else if path.is_file() {
            link_file(&path, &dest)?;
            linked += 1;
        }
    }
    Ok(linked)
}

fn link_file(src: &Path, dest: &Path) -> io::Result<()> {
    if dest.exists() {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::hard_link(src, dest).is_ok() {
        return Ok(());
    }
    fs::copy(src, dest).map(|_| ())
}

fn scrub_tree(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.is_dir() {
            scrub_tree(&path)?;
        } else if is_profile_name(name) {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn is_dep_info(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("d")
}

fn same_unit(name: &str, stem: &str) -> bool {
    name == stem
        || name
            .strip_prefix(stem)
            .is_some_and(|rest| rest.starts_with('.'))
}

fn is_profile_name(name: &str) -> bool {
    name.ends_with(".profraw") || name.ends_with(".profdata")
}

fn is_registry_dep_info(text: &str, workspace: &Path) -> bool {
    let hay = text.replace('\\', "/");
    if contains_path(&hay, &slash_path(&workspace.join("crates"))) {
        return false;
    }
    contains_path(&hay, "/.cargo/registry/")
        || contains_path(&hay, "/.cargo/git/")
        || contains_path(&hay, "/cargo/registry/")
        || contains_path(&hay, "/cargo/git/")
}

fn contains_path(hay: &str, needle: &str) -> bool {
    if cfg!(windows) {
        hay.to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase())
    } else {
        hay.contains(needle)
    }
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::{
        is_registry_dep_info, resolve_llvm_cov_target, scrub_profiles, seed_registry_artifacts,
    };
    use std::fs;
    use std::path::Path;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    #[test]
    fn the_llvm_cov_target_follows_an_explicit_directory_or_cargo_target() {
        let root = Path::new("repo");
        assert_eq!(
            resolve_llvm_cov_target(root, None, None),
            root.join("target").join("llvm-cov-target")
        );
        assert_eq!(
            resolve_llvm_cov_target(root, None, Some(Path::new("custom"))),
            root.join("custom").join("llvm-cov-target")
        );
        let explicit = std::env::temp_dir().join("cov-target");
        assert_eq!(
            resolve_llvm_cov_target(root, Some(&explicit), None),
            explicit
        );
    }

    #[test]
    fn a_seed_hardlinks_registry_artifacts_and_skips_workspace_and_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("repo");
        let from = workspace.join("target").join("llvm-cov-target");
        let registry = dir.path().join("home").join(".cargo").join("registry");
        let serde_d = format!(
            "{}: {}/serde/src/lib.rs\n",
            from.join("debug/deps/libserde-aaa.rlib").display(),
            registry.display()
        );
        let own_d = format!(
            "{}: {}/src/lib.rs\n",
            from.join("debug/deps/libliberado_cli-bbb.rlib").display(),
            workspace.join("crates").join("cli").display()
        );
        write(&from.join("debug/deps/libserde-aaa.d"), &serde_d);
        write(&from.join("debug/deps/libserde-aaa.rlib"), "serde-rlib");
        write(&from.join("debug/deps/libliberado_cli-bbb.d"), &own_d);
        write(
            &from.join("debug/deps/libliberado_cli-bbb.rlib"),
            "workspace-rlib",
        );
        write(&from.join("debug/deps/run-123.profraw"), "profraw");
        write(
            &from.join("debug/.fingerprint/serde-aaa/dep-info"),
            &serde_d,
        );
        write(
            &from.join("debug/.fingerprint/serde-aaa/invoked.timestamp"),
            "1",
        );
        write(
            &from.join("debug/.fingerprint/liberado_cli-bbb/dep-info"),
            &own_d,
        );
        write(&from.join("debug/build/serde-aaa/output"), "build-out");
        write(
            &from.join("debug/build/liberado_cli-bbb/output"),
            "own-build",
        );

        let to = dir.path().join("sibling");
        let linked = seed_registry_artifacts(&from, &to, &workspace).unwrap();
        assert!(linked >= 3, "linked {linked}");
        assert_eq!(
            fs::read_to_string(to.join("debug/deps/libserde-aaa.rlib")).unwrap(),
            "serde-rlib"
        );
        assert!(!to.join("debug/deps/libliberado_cli-bbb.rlib").exists());
        assert!(!to.join("debug/deps/run-123.profraw").exists());
        assert!(
            to.join("debug/.fingerprint/serde-aaa/invoked.timestamp")
                .is_file()
        );
        assert!(
            !to.join("debug/.fingerprint/liberado_cli-bbb/dep-info")
                .exists()
        );
        assert_eq!(
            fs::read_to_string(to.join("debug/build/serde-aaa/output")).unwrap(),
            "build-out"
        );
        assert!(!to.join("debug/build/liberado_cli-bbb/output").exists());
        assert!(is_registry_dep_info(&serde_d, &workspace));
        assert!(!is_registry_dep_info(&own_d, &workspace));
    }

    #[test]
    fn scrub_profiles_deletes_profraw_and_profdata_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("debug/deps/libserde-aaa.rlib"), "keep");
        write(&root.join("debug/old.profraw"), "drop");
        write(&root.join("name.profdata"), "drop");
        scrub_profiles(root).unwrap();
        assert!(root.join("debug/deps/libserde-aaa.rlib").is_file());
        assert!(!root.join("debug/old.profraw").exists());
        assert!(!root.join("name.profdata").exists());
    }
}
