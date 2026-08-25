//! Mutation-testing campaign ledger: run, record, report, and next-crate selection.
//!
//! The ledger at `mutants-ledger.json` is append-only. Each completed `cargo mutants` run adds one
//! row. Squashing survivors means re-running and appending — never editing prior rows.

use crate::crate_map_cmd::{self, CrateInfo};
use chrono::Utc;
use liberado_common::process::std_command;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub const LEDGER_FILE: &str = "mutants-ledger.json";
const OUTCOMES_FILE: &str = "mutants.out/outcomes.json";
const MUTANTS_TARGET_DIR: &str = "target/mutants";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Counts {
    pub viable: u32,
    pub caught: u32,
    pub survived: u32,
    pub timeout: u32,
    pub unviable: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Campaign {
    pub package: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    pub recorded_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_version: Option<String>,
    pub scope: String,
    pub counts: Counts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ledger {
    pub schema: u32,
    pub campaigns: Vec<Campaign>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunProfile {
    Default,
    LibOnly,
}

#[path = "mutants_cmd_run.rs"]
mod run_support;
use run_support::{clear_stale_outcomes, parse_run_invocation};

pub fn run(args: &mut impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let root = crate_map_cmd::repository_root()?;
    let arguments: Vec<String> = args.collect();
    let invocation = parse_run_invocation(&arguments)?;
    let crate_info = resolve_crate(&root, &invocation.crate_dir)?;
    clear_stale_outcomes(&root)?;

    let command = build_mutants_command(&crate_info.name, invocation.profile);
    eprintln!("[mutants] running: {command}");
    let status = run_support::spawn_mutants(&root, &command)?;
    run_support::announce_record(
        record_campaign(
            &root,
            Some(&invocation.crate_dir),
            Some(&command),
            invocation.profile,
        )?,
        status.success(),
    );
    Ok(())
}

pub fn record(args: &mut impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let root = crate_map_cmd::repository_root()?;
    let crate_dir = args.next();
    if args.next().is_some() {
        return Err("usage: liberado mutants record [crate-dir]".into());
    }
    let profile = crate_dir
        .as_deref()
        .map(|dir| {
            if dir == "coder-agent" {
                RunProfile::LibOnly
            } else {
                RunProfile::Default
            }
        })
        .unwrap_or(RunProfile::Default);
    match record_campaign(&root, crate_dir.as_deref(), None, profile)? {
        RecordOutcome::Appended { package, commit } => {
            println!("Recorded campaign for {package} at {commit}");
        }
        RecordOutcome::SkippedIncomplete => {
            return Err(format!(
                "no complete outcomes at {}/{}; run cargo mutants first",
                root.display(),
                OUTCOMES_FILE
            )
            .into());
        }
    }
    Ok(())
}

pub fn report(args: &mut impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let root = crate_map_cmd::repository_root()?;
    let include_all =
        run_support::parse_include_all(args, "usage: liberado mutants report [--all]")?;
    let ledger = load_ledger(&root)?;
    let crates = crate_map_cmd::list_crates(&root)?;
    let health = build_health(&root, &ledger, &crates, include_all)?;
    print!("{}", render_report(&health));
    Ok(())
}

pub fn next_crate(
    args: &mut impl Iterator<Item = String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let root = crate_map_cmd::repository_root()?;
    let include_all = run_support::parse_include_all(args, "usage: liberado mutants next [--all]")?;
    let ledger = load_ledger(&root)?;
    let crates = crate_map_cmd::list_crates(&root)?;
    let health = build_health(&root, &ledger, &crates, include_all)?;
    if let Some(name) = health
        .never_campaigned
        .first()
        .map(|entry| entry.dir.as_str())
    {
        println!("{name}");
        return Ok(());
    }
    if let Some(entry) = health.most_drift.first() {
        println!("{}", entry.dir);
        return Ok(());
    }
    if let Some(entry) = health.historical_only.first() {
        println!("{}", entry.dir);
        return Ok(());
    }
    Err("no crates matched the selection filters".into())
}

#[derive(Debug)]
enum RecordOutcome {
    Appended { package: String, commit: String },
    SkippedIncomplete,
}

/// Per-crate `(test_timeout, minimum_test_timeout)` overrides.
///
/// Data, not match arms: each new crate entry is a row here rather than a
/// branch in [`build_mutants_command`], which sits just under its
/// function-complexity ratchet and must not grow with the campaign list.
const TIMEOUT_OVERRIDES: &[(&str, &str, &str)] = &[
    // liberado-cli pulls most of the workspace; baseline + integration tests exceed 3s.
    ("liberado-cli", "120", "120"),
    // memory-mcp's stdio integration tests exceed 3s on a cold target/mutants cache,
    // which times out the unmutated baseline and kills the whole campaign.
    ("liberado-memory-mcp", "60", "60"),
    // Same cold-cache effect for conversation-store: the baseline test phase also
    // compiles doctests, which alone exceeds the 3s floor on a cold cache.
    ("liberado-conversation-store", "60", "60"),
    // coder-core's suite is simply large; a cold cache pushes the baseline test
    // phase well past 3s before any mutant runs.
    ("liberado-coder-core", "90", "90"),
    // acp-bridge spawns child processes in its smoke test; a cold baseline exceeds 3s.
    ("liberado-acp-bridge", "10.0", "120"),
    // chat-search pulls the Tantivy index stack; its cold-cache baseline test phase
    // exceeds 3s before any mutant runs (same signature as conversation-store).
    ("liberado-chat-search", "60", "60"),
    // coder-tools shells out to real git in its tests; 141/582 mutants timed
    // out at the 3s floor without ever being decided.
    ("liberado-coder-tools", "30", "30"),
];

fn build_mutants_command(package: &str, profile: RunProfile) -> String {
    // Plain loop, no closures: this file sits against a function-count
    // ratchet, so table lookups must not cost lambda entries.
    let mut override_pair: Option<(&'static str, &'static str)> = None;
    for entry in TIMEOUT_OVERRIDES {
        if entry.0 == package {
            override_pair = Some((entry.1, entry.2));
        }
    }
    let (test_timeout, min_test_timeout) = override_pair.unwrap_or(match profile {
        RunProfile::LibOnly => ("90", "90"),
        _ => ("3.0", "30"),
    });
    let mut parts = vec![
        "cargo".to_string(),
        "mutants".to_string(),
        "-p".to_string(),
        package.to_string(),
        "--cap-lints".to_string(),
        "true".to_string(),
        "--timeout".to_string(),
        test_timeout.to_string(),
        "--minimum-test-timeout".to_string(),
        min_test_timeout.to_string(),
    ];
    // cargo-mutants copies the workspace into %TEMP%; paseo/node_modules symlinks fail on
    // Windows without elevation (os error 1314), and on any host the gitignored sibling
    // checkouts (turbovault/, turbomcp/) do not survive the copy, so manifest resolution
    // fails in the temp dir. In-place avoids both. Recovery ritual lives in Skills/mutants-campaign.md
    // — prefer this to a run that never starts.
    parts.extend(["--in-place".into()]);
    if profile == RunProfile::LibOnly && package != "liberado-cli" {
        parts.extend(["--".into(), "--lib".into()]);
    }
    parts.join(" ")
}

fn record_campaign(
    root: &Path,
    crate_dir: Option<&str>,
    command: Option<&str>,
    profile: RunProfile,
) -> Result<RecordOutcome, Box<dyn std::error::Error>> {
    let outcomes_path = root.join(OUTCOMES_FILE);
    if !outcomes_path.is_file() {
        return Ok(RecordOutcome::SkippedIncomplete);
    }
    let bytes = fs::read(&outcomes_path)?;
    let outcomes: OutcomesFile = serde_json::from_slice(&bytes)?;
    if outcomes.cargo_mutants_version.is_empty() {
        return Ok(RecordOutcome::SkippedIncomplete);
    }
    let package = package_from_outcomes_bytes(&bytes)
        .or_else(|| crate_dir.and_then(|dir| resolve_crate(root, dir).ok().map(|info| info.name)))
        .ok_or("could not determine package name from outcomes or crate directory")?;
    if let Some(dir) = crate_dir {
        let expected = resolve_crate(root, dir)?;
        if expected.name != package {
            return Err(format!(
                "outcomes package {package} does not match crate directory {dir} ({})",
                expected.name
            )
            .into());
        }
    }
    let command = command
        .map(str::to_string)
        .unwrap_or_else(|| build_mutants_command(&package, profile));
    let scope = if command.contains("--file") {
        "file".to_string()
    } else {
        "package".to_string()
    };
    let commit = current_commit(root)?;
    let counts = outcomes.counts();
    if let Some(skip) = validate_outcomes(&counts, outcomes.total_mutants) {
        return Ok(skip);
    }
    let campaign = Campaign {
        package: package.clone(),
        commit: Some(commit.clone()),
        recorded_at: Utc::now().format("%Y-%m-%d").to_string(),
        command: Some(command),
        tool_version: Some(outcomes.cargo_mutants_version),
        scope,
        counts,
        source: None,
    };
    append_campaign(root, campaign)?;
    Ok(RecordOutcome::Appended { package, commit })
}

fn append_campaign(root: &Path, campaign: Campaign) -> Result<(), Box<dyn std::error::Error>> {
    let mut ledger = load_ledger(root)?;
    ledger.campaigns.push(campaign);
    save_ledger(root, &ledger)
}

pub fn load_ledger(root: &Path) -> Result<Ledger, Box<dyn std::error::Error>> {
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

/// Every tested mutant lands in exactly one of the four buckets; when the
/// declared total is known and the buckets do not sum to it, the outcomes file
/// is a partial write from a killed run.
/// Refuse outcome files that would shadow the crate's real last campaign:
/// zero-viable means the baseline build never happened, and a bucket sum below
/// the declared total means the run was killed mid-campaign.
fn validate_outcomes(counts: &Counts, total_mutants: u32) -> Option<RecordOutcome> {
    if counts.viable == 0 {
        return Some(RecordOutcome::SkippedIncomplete);
    }
    // A missing declared total (older outcomes format) cannot prove the run
    // finished; treating 0 as "no check" would record partial campaigns.
    if total_mutants == 0 {
        return Some(RecordOutcome::SkippedIncomplete);
    }
    let accounted = counts.caught + counts.survived + counts.timeout + counts.unviable;
    if accounted != total_mutants {
        return Some(RecordOutcome::SkippedIncomplete);
    }
    None
}

// The body lives in run_support::persist_ledger; this delegate keeps the
// historical name, call sites, and score at the root.
fn save_ledger(root: &Path, ledger: &Ledger) -> Result<(), Box<dyn std::error::Error>> {
    run_support::persist_ledger(root, ledger)
}

fn resolve_crate(root: &Path, crate_dir: &str) -> Result<CrateInfo, Box<dyn std::error::Error>> {
    crate_map_cmd::list_crates(root)?
        .into_iter()
        .find(|info| info.dir == crate_dir)
        .ok_or_else(|| {
            format!("unknown crate directory {crate_dir:?}; use the crates/ folder name").into()
        })
}

fn current_commit(root: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let output = std_command("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err("git rev-parse HEAD failed; is this a git repository?".into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

#[derive(Debug, Deserialize)]
struct OutcomesFile {
    #[serde(default)]
    total_mutants: u32,
    caught: u32,
    missed: u32,
    timeout: u32,
    unviable: u32,
    cargo_mutants_version: String,
}

fn package_from_outcomes_bytes(bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    value.get("outcomes")?.as_array()?.iter().find_map(|entry| {
        entry
            .pointer("/scenario/Mutant/package")
            .and_then(|package| package.as_str())
            .map(str::to_string)
    })
}

impl OutcomesFile {
    fn counts(&self) -> Counts {
        let viable = self.caught + self.missed + self.timeout;
        Counts {
            viable,
            caught: self.caught,
            survived: self.missed,
            timeout: self.timeout,
            unviable: self.unviable,
        }
    }
}

#[derive(Debug, Clone)]
struct CrateHealthEntry {
    dir: String,
    role: String,
    latest_commit: Option<String>,
    commits_since: Option<u64>,
    lines_changed: Option<String>,
    drift_note: Option<String>,
    latest_counts: Option<Counts>,
}

#[derive(Debug, Clone)]
struct HealthReport {
    never_campaigned: Vec<CrateHealthEntry>,
    historical_only: Vec<CrateHealthEntry>,
    most_drift: Vec<CrateHealthEntry>,
}

fn build_health(
    root: &Path,
    ledger: &Ledger,
    crates: &[CrateInfo],
    include_all: bool,
) -> Result<HealthReport, Box<dyn std::error::Error>> {
    let package_campaigns = package_campaigns_by_package(ledger);
    let mut never_campaigned = Vec::new();
    let mut historical_only = Vec::new();
    let mut most_drift = Vec::new();

    for info in crates {
        if !include_all && matches!(info.role.as_str(), "testing" | "tooling") {
            continue;
        }
        let campaigns = package_campaigns
            .get(&info.name)
            .cloned()
            .unwrap_or_default();
        let entry = base_entry(info);
        if campaigns.is_empty() {
            never_campaigned.push(entry);
            continue;
        }
        let latest_sha = campaigns
            .iter()
            .rev()
            .find_map(|campaign| campaign.commit.clone());
        if latest_sha.is_none() {
            historical_only.push(enrich_historical(entry, campaigns.last()));
            continue;
        }
        let commit = latest_sha.unwrap();
        let enriched = enrich_drift(root, entry, &commit, campaigns.last())?;
        most_drift.push(enriched);
    }

    never_campaigned.sort_by(|a, b| a.dir.cmp(&b.dir));
    historical_only.sort_by(|a, b| a.dir.cmp(&b.dir));
    most_drift.sort_by(|a, b| {
        b.commits_since
            .unwrap_or(0)
            .cmp(&a.commits_since.unwrap_or(0))
            .then_with(|| a.dir.cmp(&b.dir))
    });

    Ok(HealthReport {
        never_campaigned,
        historical_only,
        most_drift,
    })
}

fn package_campaigns_by_package(ledger: &Ledger) -> BTreeMap<String, Vec<&Campaign>> {
    let mut grouped: BTreeMap<String, Vec<&Campaign>> = BTreeMap::new();
    // Zero-viable rows (crashed/partial runs recorded before the recorder's
    // refusal existed) are skipped via the is_health_row filter below so they
    // cannot shadow the real campaign.
    for campaign in ledger
        .campaigns
        .iter()
        .filter(|c| c.scope == "package" && c.counts.viable > 0)
    {
        grouped
            .entry(campaign.package.clone())
            .or_default()
            .push(campaign);
    }
    grouped
}

fn base_entry(info: &CrateInfo) -> CrateHealthEntry {
    CrateHealthEntry {
        dir: info.dir.clone(),
        role: info.role.clone(),
        latest_commit: None,
        commits_since: None,
        lines_changed: None,
        drift_note: None,
        latest_counts: None,
    }
}

fn enrich_historical(mut entry: CrateHealthEntry, latest: Option<&&Campaign>) -> CrateHealthEntry {
    if let Some(campaign) = latest {
        entry.latest_counts = Some(campaign.counts.clone());
        entry.drift_note = campaign
            .source
            .clone()
            .or_else(|| Some("historical".into()));
    }
    entry
}

fn enrich_drift(
    root: &Path,
    mut entry: CrateHealthEntry,
    commit: &str,
    latest: Option<&&Campaign>,
) -> Result<CrateHealthEntry, Box<dyn std::error::Error>> {
    entry.latest_commit = Some(short_sha(commit));
    if let Some(campaign) = latest {
        entry.latest_counts = Some(campaign.counts.clone());
    }
    if !commit_is_ancestor(root, commit)? {
        entry.drift_note = Some("commit not in this history".into());
        return Ok(entry);
    }
    let crate_path = format!("crates/{}", entry.dir);
    entry.commits_since = Some(rev_list_count(root, commit, &crate_path)?);
    entry.lines_changed = git_shortstat(root, commit, &crate_path).ok();
    Ok(entry)
}

fn short_sha(commit: &str) -> String {
    commit.chars().take(12).collect()
}

fn commit_is_ancestor(root: &Path, commit: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let output = std_command("git")
        .args(["merge-base", "--is-ancestor", commit, "HEAD"])
        .current_dir(root)
        .output()?;
    Ok(output.status.success())
}

fn rev_list_count(
    root: &Path,
    commit: &str,
    path: &str,
) -> Result<u64, Box<dyn std::error::Error>> {
    let range = format!("{commit}..HEAD");
    let output = std_command("git")
        .args(["rev-list", "--count", &range, "--", path])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git rev-list failed for {path}: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().parse()?)
}

fn git_shortstat(
    root: &Path,
    commit: &str,
    path: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let range = format!("{commit}..HEAD");
    let output = std_command("git")
        .args(["diff", "--shortstat", &range, "--", path])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git diff --shortstat failed for {path}: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

/// Render the campaign health report as text.
///
/// Pure so tests can assert on it directly; `report` prints the result. Each
/// section has its own renderer so tests can pin one section's shape without
/// building the others.
fn render_report(health: &HealthReport) -> String {
    let mut out = String::from("=== Mutants campaign health ===\n\n");
    out.push_str(&render_never_campaigned(&health.never_campaigned));
    out.push_str(&render_historical_only(&health.historical_only));
    out.push_str(&render_most_drift(&health.most_drift));
    out
}

/// Render the "historical only" section.
/// Render the "never campaigned" section.
fn render_never_campaigned(entries: &[CrateHealthEntry]) -> String {
    let mut out = format!("\nNever campaigned ({}):\n", entries.len());
    if entries.is_empty() {
        out.push_str("  (none)\n");
        return out;
    }
    for entry in entries {
        out.push_str(&format!("  {} [{}]\n", entry.dir, entry.role));
    }
    out
}

fn render_historical_only(entries: &[CrateHealthEntry]) -> String {
    let mut out = format!("\nHistorical only — no commit SHA ({}):\n", entries.len());
    if entries.is_empty() {
        out.push_str("  (none)\n");
        return out;
    }
    for entry in entries {
        let counts = entry
            .latest_counts
            .as_ref()
            .map(format_counts)
            .unwrap_or_default();
        out.push_str(&format!("  {} [{}]{}\n", entry.dir, entry.role, counts));
    }
    out
}

/// Render the "most drift" section.
fn render_most_drift(entries: &[CrateHealthEntry]) -> String {
    let mut out = format!(
        "\nMost drift since last SHA campaign ({}):\n",
        entries.len()
    );
    if entries.is_empty() {
        out.push_str("  (none)\n");
        return out;
    }
    for entry in entries {
        if let Some(note) = &entry.drift_note {
            out.push_str(&format!("  {} [{}] — {}\n", entry.dir, entry.role, note));
            continue;
        }
        let lines = entry
            .lines_changed
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or("0 files changed");
        let counts = entry
            .latest_counts
            .as_ref()
            .map(format_counts)
            .unwrap_or_default();
        out.push_str(&format!(
            "  {} [{}] — {} commits since {} — {}{}\n",
            entry.dir,
            entry.role,
            entry.commits_since.unwrap_or(0),
            entry.latest_commit.as_deref().unwrap_or("?"),
            lines,
            counts
        ));
    }
    out
}

fn format_counts(counts: &Counts) -> String {
    format!(
        " — viable {} caught {} survived {} timeout {}",
        counts.viable, counts.caught, counts.survived, counts.timeout
    )
}

#[cfg(test)]
#[path = "mutants_cmd_tests.rs"]
mod tests;
