//! Source-root identity for a managed ordinary Cargo cache.
//!
//! Lives in its own file so `cargo_targets.rs` stays under the new-file
//! module-health ceiling. Hashing is not allocation.

use std::path::{Component, Path, PathBuf};

/// Stable directory segment for one source root under `shared/`.
pub(super) fn source_key(path: &Path) -> String {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let normalized = fold_canonical_path(&canon, component_is_case_insensitive);
    format!("{:016x}", fnv1a64(normalized.as_bytes()))
}

/// Fold case only on components that are themselves case-insensitive.
///
/// A case-insensitive ancestor must not lowercase a case-sensitive leaf.
/// `/Volumes` on default macOS can sit above a case-sensitive volume that
/// holds distinct `Repo` and `repo` directories.
pub(super) fn fold_canonical_path(
    path: &Path,
    component_insensitive: impl Fn(&Path) -> bool,
) -> String {
    let mut acc = PathBuf::new();
    let mut out = String::new();
    for component in path.components() {
        acc.push(component);
        push_folded_component(&mut out, component, &acc, &component_insensitive);
    }
    out.replace('\\', "/")
}

fn push_folded_component(
    out: &mut String,
    component: Component<'_>,
    acc: &Path,
    component_insensitive: &impl Fn(&Path) -> bool,
) {
    match component {
        Component::Prefix(prefix) => {
            out.push_str(&prefix.as_os_str().to_string_lossy().to_ascii_lowercase());
        }
        Component::RootDir => {
            if out.is_empty() || out.ends_with(':') {
                out.push('/');
            }
        }
        Component::Normal(name) => {
            if !out.is_empty() && !out.ends_with('/') {
                out.push('/');
            }
            let raw = name.to_string_lossy();
            if component_insensitive(acc) {
                out.push_str(&raw.to_ascii_lowercase());
            } else {
                out.push_str(&raw);
            }
        }
        Component::CurDir => {
            if !out.is_empty() && !out.ends_with('/') {
                out.push('/');
            }
            out.push('.');
        }
        Component::ParentDir => {
            if !out.is_empty() && !out.ends_with('/') {
                out.push('/');
            }
            out.push_str("..");
        }
    }
}

/// True when this path's own name has a case-flipped alias to the same
/// directory. Ancestors are not consulted.
fn component_is_case_insensitive(path: &Path) -> bool {
    let Some(flipped) = flipped_ascii_name(path) else {
        return false;
    };
    let other = path.with_file_name(flipped);
    other.exists() && same_canonical(path, &other)
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

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}
