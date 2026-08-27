//! Ledger persistence for the mutation-campaign commands: locked append, load,
//! dedupe-on-save. Split from `mutants_cmd.rs` to hold its own complexity;
//! everything here is private to the `mutants_cmd` module tree.

use super::{Campaign, LEDGER_FILE, Ledger};
use fs4::fs_std::FileExt;
use std::fs::{self, File, OpenOptions};
use std::path::Path;

pub(super) fn append_campaign(
    root: &Path,
    campaign: Campaign,
) -> Result<(), Box<dyn std::error::Error>> {
    // The rename in `write_atomic` makes one write indivisible, but the whole
    // update is read-modify-write. Two agents appending at once could each read
    // the same ledger and the last rename would drop the other's row — the
    // exact concurrent-agent workflow the skill documents. Hold an OS lock
    // (BSD flock / Windows LockFileEx via fs4) across the sequence; the kernel
    // releases it if a process dies, so no stale lock can wedge later runs.
    let _guard = ledger_lock(root)?;
    let mut ledger = load_ledger(root)?;
    ledger.campaigns.push(campaign);
    save_ledger(root, &ledger)
}

/// Cross-process exclusive hold over the ledger read-modify-write window.
/// Blocking acquisition is deliberate: the critical section is two small file
/// operations, so waiting always beats failing a campaign that took minutes.
struct LedgerLock(File);

impl Drop for LedgerLock {
    fn drop(&mut self) {
        // Explicit unlock before close: closing would release anyway, but
        // naming it keeps the invariant visible and the handle read.
        let _ = FileExt::unlock(&self.0);
    }
}

fn ledger_lock(root: &Path) -> Result<LedgerLock, Box<dyn std::error::Error>> {
    let path = root.join(format!("{LEDGER_FILE}.lock"));
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    FileExt::lock_exclusive(&file)?;
    Ok(LedgerLock(file))
}

pub(crate) fn load_ledger(root: &Path) -> Result<Ledger, Box<dyn std::error::Error>> {
    let path = root.join(LEDGER_FILE);
    if !path.is_file() {
        return Ok(Ledger {
            schema: 1,
            campaigns: Vec::new(),
        });
    }
    let ledger: Ledger = serde_json::from_slice(&fs::read(path)?)?;
    if ledger.schema != 1 {
        return Err(format!("unsupported {} schema {}", LEDGER_FILE, ledger.schema).into());
    }
    Ok(ledger)
}

/// Drop byte-identical duplicate rows before saving.
///
/// The ledger is append-only, but merges have pasted whole blocks twice (13
/// duplicates at once during the campaign branch). A merge that unions both
/// sides then re-saves goes through here, so the on-disk artifact can never
/// keep a duplicate pair even when git history briefly contained one.
fn dedupe_campaigns(campaigns: Vec<Campaign>) -> Vec<Campaign> {
    let mut seen = std::collections::HashSet::new();
    campaigns
        .into_iter()
        .filter(|c| {
            let canonical = serde_json::to_string(c).expect("a ledger row serialises");
            seen.insert(canonical)
        })
        .collect()
}

pub(super) fn save_ledger(root: &Path, ledger: &Ledger) -> Result<(), Box<dyn std::error::Error>> {
    let ledger = Ledger {
        schema: ledger.schema,
        campaigns: dedupe_campaigns(ledger.campaigns.clone()),
    };
    let bytes = serde_json::to_string_pretty(&ledger)? + "\n";
    write_atomic(root, &bytes)?;
    Ok(())
}

// Atomic temp+rename with a pid-unique temp name. A failed write or rename
// propagates to the caller (a campaign that cannot be recorded must fail the
// command, not panic mid-run) and removes the inert `.tmp` it may leave.
fn write_atomic(root: &Path, bytes: &str) -> Result<(), Box<dyn std::error::Error>> {
    let tmp = root.join(format!("{LEDGER_FILE}.{}.tmp", std::process::id()));
    // Plain `and_then` + a function path, not a closure chain: this file sits
    // against a function-count ratchet.
    let outcome = fs::write(&tmp, bytes).and_then(|()| fs::rename(&tmp, root.join(LEDGER_FILE)));
    if outcome.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    outcome.map_err(std::convert::Into::into)
}
#[cfg(test)]
#[path = "mutants_cmd_ledger_tests.rs"]
mod survivor_tests;
