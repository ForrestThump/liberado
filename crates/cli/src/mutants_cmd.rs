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

pub fn run(args: &mut impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let root = crate_map_cmd::repository_root()?;
    let mut arguments: Vec<String> = args.collect();
    let mut profile = RunProfile::Default;
    if arguments.first().is_some_and(|flag| flag == "--lib-only") {
        profile = RunProfile::LibOnly;
        arguments.remove(0);
    }
    let crate_dir = arguments
        .first()
        .ok_or("usage: liberado mutants run [--lib-only] <crate-dir>")?
        .as_str();
    if arguments.len() > 1 {
        return Err("usage: liberado mutants run [--lib-only] <crate-dir>".into());
    }
    let crate_info = resolve_crate(&root, crate_dir)?;
    let command = build_mutants_command(&crate_info.name, profile);
    let mutants_target = root.join(MUTANTS_TARGET_DIR);
    // outcomes.json is persistent scratch. If this run dies before cargo-mutants rewrites it,
    // the file still holds the previous campaign of (often) this same crate; recording would
    // then append those stale counts under today's commit and reset the drift clock. Remove
    // it first so a row can only ever come from the run that just finished.
    let _ = fs::remove_file(root.join(OUTCOMES_FILE));
    eprintln!("[mutants] running: {command}");
    eprintln!(
        "[mutants] artifact dir: {} (isolated from target/debug)",
        mutants_target.display()
    );
    let status = std_command("cargo")
        .args(command.split_whitespace().skip(1))
        .current_dir(&root)
        .env("CARGO_TARGET_DIR", &mutants_target)
        .status()?;
    match record_campaign(&root, Some(crate_dir), Some(&command), profile)? {
        RecordOutcome::Appended { package, commit } => {
            eprintln!("[mutants] recorded campaign for {package} at {commit}");
        }
        RecordOutcome::SkippedIncomplete => {
            eprintln!("[mutants] run finished but outcomes were incomplete; nothing recorded");
        }
    }
    if !status.success() {
        eprintln!(
            "[mutants] cargo mutants exited with {}; campaign recorded if outcomes were complete",
            status
        );
    }
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
    let include_all = args.next().is_some_and(|flag| flag == "--all");
    if args.next().is_some() {
        return Err("usage: liberado mutants report [--all]".into());
    }
    let ledger = load_ledger(&root)?;
    let crates = crate_map_cmd::list_crates(&root)?;
    let health = build_health(&root, &ledger, &crates, include_all)?;
    print_report(&health);
    Ok(())
}

pub fn next_crate(
    args: &mut impl Iterator<Item = String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let root = crate_map_cmd::repository_root()?;
    let include_all = args.next().is_some_and(|flag| flag == "--all");
    if args.next().is_some() {
        return Err("usage: liberado mutants next [--all]".into());
    }
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

enum RecordOutcome {
    Appended { package: String, commit: String },
    SkippedIncomplete,
}

fn build_mutants_command(package: &str, profile: RunProfile) -> String {
    let (test_timeout, min_test_timeout) = match (package, profile) {
        // liberado-cli pulls most of the workspace; baseline + integration tests exceed 3s.
        ("liberado-cli", _) => ("120", "120"),
        // memory-mcp's stdio integration tests exceed 3s on a cold target/mutants cache,
        // which times out the unmutated baseline and kills the whole campaign.
        ("liberado-memory-mcp", _) => ("60", "60"),
        // Same cold-cache effect for conversation-store: the baseline test phase also
        // compiles doctests, which alone exceeds the 3s floor on a cold cache.
        ("liberado-conversation-store", _) => ("60", "60"),
        // coder-core's suite is simply large; a cold cache pushes the baseline test
        // phase well past 3s before any mutant runs.
        ("liberado-coder-core", _) => ("90", "90"),
        // acp-bridge spawns child processes in its smoke test; a cold baseline exceeds 3s.
        ("liberado-acp-bridge", _) => ("10.0", "120"),
        (_, RunProfile::LibOnly) => ("90", "90"),
        _ => ("3.0", "30"),
    };
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
    // fails in the temp dir. In-place avoids both. Restore risk is documented in AGENTS.md
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
    if counts.viable == 0 {
        // A completed outcomes file with no viable mutants means cargo-mutants never got a
        // baseline build (missing sibling checkout, disk full, config error). Recording it
        // would shadow the crate's real last campaign — the report reads the newest row.
        return Ok(RecordOutcome::SkippedIncomplete);
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

fn save_ledger(root: &Path, ledger: &Ledger) -> Result<(), Box<dyn std::error::Error>> {
    let path = root.join(LEDGER_FILE);
    // Temp file + rename: the ledger is append-only history, and a crash mid-write must
    // not leave a truncated file that would read as "all campaigns lost". A failed rename
    // may leave a stray `.tmp` beside it; that is inert scratch, unlike a truncated ledger.
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(ledger)? + "\n")?;
    fs::rename(tmp, path).map_err(Into::into)
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
    for campaign in &ledger.campaigns {
        if campaign.scope != "package" {
            continue;
        }
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

fn print_report(health: &HealthReport) {
    println!("=== Mutants campaign health ===");
    print!(
        "{}{}{}",
        render_never_campaigned(&health.never_campaigned),
        render_historical_only(&health.historical_only),
        render_most_drift(&health.most_drift)
    );
}

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

/// Render the "historical only" section.
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
mod tests {
    use super::*;
    use std::process::Command;

    fn init_git_repo(root: &Path) {
        for (dir, name) in [
            ("crates/alpha", "liberado-alpha"),
            ("crates/beta", "liberado-beta"),
        ] {
            fs::create_dir_all(root.join(dir)).unwrap();
            fs::write(
                root.join(dir).join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{name}\"\n\n[package.metadata.liberado]\nrole = \"kernel\"\n"
                ),
            )
            .unwrap();
            fs::write(
                root.join(dir).join("lib.rs"),
                "pub fn value() -> i32 { 1 }\n",
            )
            .unwrap();
        }
        run_git(root, &["init"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "Test"]);
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", "initial"]);
    }

    fn run_git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .expect("git command");
        assert!(status.success(), "git {:?} failed", args);
    }

    #[test]
    fn ingest_counts_from_outcomes_json() {
        let outcomes: OutcomesFile = serde_json::from_str(
            r#"{
  "caught": 3,
  "missed": 1,
  "timeout": 0,
  "unviable": 2,
  "cargo_mutants_version": "27.1.0"
}"#,
        )
        .unwrap();
        let counts = outcomes.counts();
        assert_eq!(counts.viable, 4);
        assert_eq!(counts.caught, 3);
        assert_eq!(counts.survived, 1);
        assert_eq!(counts.unviable, 2);
    }

    #[test]
    fn package_from_outcomes_skips_baseline_row() {
        let package = package_from_outcomes_bytes(
            br#"{
  "outcomes": [
    {"scenario": "Baseline"},
    {"scenario": {"Mutant": {"package": "liberado-alpha"}}}
  ],
  "caught": 1,
  "missed": 0,
  "timeout": 0,
  "unviable": 0,
  "cargo_mutants_version": "27.1.0"
}"#,
        );
        assert_eq!(package, Some("liberado-alpha".into()));
    }

    #[test]
    fn record_refuses_zero_viable_outcomes_so_a_crashed_run_cannot_shadow_the_last_campaign() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_git_repo(root);
        // A completed outcomes file from a run whose baseline build never happened: every
        // count zero. Recording it would append an all-zero row that the report treats as
        // the crate's newest campaign.
        fs::create_dir_all(root.join("mutants.out")).unwrap();
        fs::write(
            root.join(OUTCOMES_FILE),
            r#"{
  "caught": 0,
  "missed": 0,
  "timeout": 0,
  "unviable": 0,
  "cargo_mutants_version": "27.1.0"
}"#,
        )
        .unwrap();

        let outcome = record_campaign(root, Some("alpha"), None, RunProfile::Default).unwrap();
        assert!(matches!(outcome, RecordOutcome::SkippedIncomplete));
        assert!(
            load_ledger(root).unwrap().campaigns.is_empty(),
            "a zero-viable run must not append a ledger row"
        );
    }

    #[test]
    fn ledger_append_preserves_prior_rows() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join(LEDGER_FILE),
            r#"{"schema":1,"campaigns":[{"package":"liberado-alpha","commit":null,"recorded_at":"2026-07-29","scope":"package","source":"markdown-seed","counts":{"viable":1,"caught":1,"survived":0,"timeout":0,"unviable":0}}]}"#,
        )
        .unwrap();
        append_campaign(
            root,
            Campaign {
                package: "liberado-alpha".into(),
                commit: Some("abc123".into()),
                recorded_at: "2026-08-21".into(),
                command: Some("cargo mutants -p liberado-alpha".into()),
                tool_version: Some("27.1.0".into()),
                scope: "package".into(),
                counts: Counts {
                    viable: 4,
                    caught: 4,
                    survived: 0,
                    timeout: 0,
                    unviable: 0,
                },
                source: None,
            },
        )
        .unwrap();
        let ledger = load_ledger(root).unwrap();
        assert_eq!(ledger.campaigns.len(), 2);
        assert!(ledger.campaigns[0].commit.is_none());
        assert_eq!(ledger.campaigns[1].commit.as_deref(), Some("abc123"));
    }

    #[test]
    fn report_groups_never_historical_and_drift() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_git_repo(root);
        let base = current_commit(root).unwrap();
        fs::write(
            root.join("crates/beta/lib.rs"),
            "pub fn value() -> i32 { 2 }\n",
        )
        .unwrap();
        run_git(root, &["add", "crates/beta/lib.rs"]);
        run_git(root, &["commit", "-m", "change beta"]);

        let ledger = Ledger {
            schema: 1,
            campaigns: vec![
                Campaign {
                    package: "liberado-alpha".into(),
                    commit: None,
                    recorded_at: "2026-07-29".into(),
                    command: None,
                    tool_version: Some("27.1.0".into()),
                    scope: "package".into(),
                    counts: Counts {
                        viable: 10,
                        caught: 9,
                        survived: 1,
                        timeout: 0,
                        unviable: 0,
                    },
                    source: Some("markdown-seed".into()),
                },
                Campaign {
                    package: "liberado-beta".into(),
                    commit: Some(base),
                    recorded_at: "2026-08-01".into(),
                    command: Some("cargo mutants -p liberado-beta".into()),
                    tool_version: Some("27.1.0".into()),
                    scope: "package".into(),
                    counts: Counts {
                        viable: 5,
                        caught: 4,
                        survived: 1,
                        timeout: 0,
                        unviable: 0,
                    },
                    source: None,
                },
            ],
        };
        let crates = crate_map_cmd::list_crates(root).unwrap();
        let health = build_health(root, &ledger, &crates, true).unwrap();
        assert!(health.never_campaigned.is_empty());
        assert_eq!(health.historical_only.len(), 1);
        assert_eq!(health.historical_only[0].dir, "alpha");
        assert_eq!(health.most_drift.len(), 1);
        assert_eq!(health.most_drift[0].dir, "beta");
        assert_eq!(health.most_drift[0].commits_since, Some(1));
    }

    #[test]
    fn drift_marks_missing_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_git_repo(root);
        let crates = crate_map_cmd::list_crates(root).unwrap();
        let ledger = Ledger {
            schema: 1,
            campaigns: vec![Campaign {
                package: "liberado-alpha".into(),
                commit: Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into()),
                recorded_at: "2026-08-01".into(),
                command: None,
                tool_version: None,
                scope: "package".into(),
                counts: Counts {
                    viable: 1,
                    caught: 1,
                    survived: 0,
                    timeout: 0,
                    unviable: 0,
                },
                source: None,
            }],
        };
        let health = build_health(root, &ledger, &crates, true).unwrap();
        assert_eq!(health.most_drift.len(), 1);
        assert_eq!(
            health.most_drift[0].drift_note.as_deref(),
            Some("commit not in this history")
        );
    }

    #[test]
    fn build_mutants_command_uses_longer_timeout_for_cli() {
        let cli = build_mutants_command("liberado-cli", RunProfile::Default);
        assert!(cli.contains("--timeout 120"));
        assert!(cli.contains("--minimum-test-timeout 120"));

        let tui = build_mutants_command("liberado-tui", RunProfile::Default);
        assert!(tui.contains("--timeout 3.0"));
        assert!(tui.contains("--minimum-test-timeout 30"));

        let acp = build_mutants_command("liberado-acp-bridge", RunProfile::Default);
        assert!(acp.contains("--timeout 10.0"));
        assert!(acp.contains("--minimum-test-timeout 120"));
    }

    #[test]
    fn repo_mutants_ledger_parses() {
        let root = crate_map_cmd::repository_root().expect("repository root");
        let ledger = load_ledger(&root).expect("ledger should parse");
        assert_eq!(ledger.schema, 1);
        assert!(
            !ledger.campaigns.is_empty(),
            "seed ledger should not be empty"
        );
    }

    fn health_entry(dir: &str) -> CrateHealthEntry {
        CrateHealthEntry {
            dir: dir.into(),
            role: "kernel".into(),
            latest_commit: None,
            commits_since: None,
            lines_changed: None,
            drift_note: None,
            latest_counts: None,
        }
    }

    #[test]
    fn empty_sections_render_none() {
        assert_eq!(
            render_never_campaigned(&[]),
            "\nNever campaigned (0):\n  (none)\n"
        );
        assert_eq!(
            render_historical_only(&[]),
            "\nHistorical only — no commit SHA (0):\n  (none)\n"
        );
        assert_eq!(
            render_most_drift(&[]),
            "\nMost drift since last SHA campaign (0):\n  (none)\n"
        );
    }

    #[test]
    fn never_campaigned_lists_each_dir_and_role() {
        let entries = vec![health_entry("alpha"), health_entry("beta")];
        assert_eq!(
            render_never_campaigned(&entries),
            "\nNever campaigned (2):\n  alpha [kernel]\n  beta [kernel]\n"
        );
    }

    #[test]
    fn historical_only_appends_counts_when_present_only() {
        let mut with_counts = health_entry("alpha");
        with_counts.latest_counts = Some(Counts {
            viable: 5,
            caught: 4,
            survived: 1,
            timeout: 0,
            unviable: 0,
        });
        let entries = vec![with_counts, health_entry("beta")];
        assert_eq!(
            render_historical_only(&entries),
            "\nHistorical only — no commit SHA (2):\n  \
             alpha [kernel] — viable 5 caught 4 survived 1 timeout 0\n  beta [kernel]\n"
        );
    }

    #[test]
    fn most_drift_prefers_the_drift_note_over_commit_detail() {
        let mut noted = health_entry("alpha");
        noted.drift_note = Some("commit not in this history".into());
        let mut detailed = health_entry("beta");
        detailed.latest_commit = Some("abc123def456".into());
        detailed.commits_since = Some(2);
        detailed.lines_changed = Some(String::new());
        detailed.latest_counts = Some(Counts {
            viable: 1,
            caught: 0,
            survived: 1,
            timeout: 0,
            unviable: 0,
        });
        let entries = vec![noted, detailed];
        assert_eq!(
            render_most_drift(&entries),
            "\nMost drift since last SHA campaign (2):\n  \
             alpha [kernel] — commit not in this history\n  \
             beta [kernel] — 2 commits since abc123def456 — 0 files changed — \
             viable 1 caught 0 survived 1 timeout 0\n"
        );
    }
}
