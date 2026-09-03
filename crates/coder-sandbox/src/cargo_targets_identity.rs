//! Source-root identity for a managed ordinary Cargo cache.
//!
//! Lives in its own file so `cargo_targets.rs` stays under the new-file
//! module-health ceiling. Hashing is not allocation.

use std::path::Path;

/// Stable directory segment for one source root under `shared/`.
pub(super) fn source_key(path: &Path) -> String {
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

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}
