//! Rust implementation of the unattended pull-request shepherd.
//!
//! It compares failure identities with the base commit, uses GitHub labels as durable
//! state, and starts daemon coding goals. Thus its automated repairs use the same coding
//! pack and ship preflight as an interactive coding run.

use chrono::Utc;
use liberado_common::process::std_command;
use regex::Regex;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

const RERUN: &str = "shepherd:ci-rerun";
const READY: &str = "shepherd:ready";
const BLOCKED: &str = "shepherd:blocked";

/// The numeric shepherd knobs and their environment keys.
///
/// Parsing lives in [`Limits::from_reader`], which takes the value reader as a parameter so
/// tests can exercise the malformed-value errors without touching process-global env state;
/// `from_env` is the production one-liner over `std::env`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Limits {
    max_kickbacks: usize,
    cold_reviews: usize,
    cold_turns: u32,
    max_concurrent: usize,
    poll: u64,
}

impl Limits {
    fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let get = |key: &str| std::env::var(key).ok();
        Ok(Self::from_reader(get)?)
    }

    fn from_reader<F>(get: F) -> Result<Self, String>
    where
        F: Fn(&str) -> Option<String>,
    {
        fn parse<T: std::str::FromStr>(
            get: impl Fn(&str) -> Option<String>,
            key: &str,
            default: T,
        ) -> Result<T, String> {
            match get(key) {
                None => Ok(default),
                Some(raw) => raw.parse().map_err(|_| {
                    format!("environment variable {key} must be a number, got '{raw}'")
                }),
            }
        }
        Ok(Self {
            max_kickbacks: parse(&get, "SHEPHERD_MAX_KICKBACKS", 2)?,
            cold_reviews: parse(&get, "SHEPHERD_COLD_REVIEWS", 2)?,
            cold_turns: parse(&get, "SHEPHERD_COLD_REVIEW_MAX_TURNS", 60)?,
            max_concurrent: parse(&get, "SHEPHERD_MAX_CONCURRENT", 2)?,
            poll: parse(&get, "SHEPHERD_POLL_SECONDS", 120)?,
        })
    }
}

#[derive(Clone)]
struct Config {
    root: PathBuf,
    repository: Option<String>,
    check_names: Vec<String>,
    daemon: String,
    project: String,
    base: String,
    profile: String,
    max_kickbacks: usize,
    cold_reviews: usize,
    cold_turns: u32,
    max_concurrent: usize,
    poll: u64,
}
impl Config {
    fn get(key: &str, default: &str) -> String {
        std::env::var(key).unwrap_or_else(|_| default.into())
    }
    fn load(selected_project: Option<&str>) -> Result<Self, Box<dyn std::error::Error>> {
        let limits = Limits::from_env()?;
        let mut config = Self {
            root: crate::crate_map_cmd::repository_root()?,
            repository: None,
            check_names: Vec::new(),
            daemon: Self::get("LIBERADO_SERVER", "http://localhost:4201"),
            project: Self::get("SHEPHERD_PROJECT", "liberado"),
            base: Self::get("SHEPHERD_BASE", "main"),
            profile: Self::get("SHEPHERD_PROFILE", "coding-unattended"),
            max_kickbacks: limits.max_kickbacks,
            cold_reviews: limits.cold_reviews,
            cold_turns: limits.cold_turns,
            max_concurrent: limits.max_concurrent,
            poll: limits.poll,
        };
        let topology = load_shepherd_topology()?;
        validate_shepherd_topology(&topology)?;
        if let Some(project) =
            select_shepherd_project(selected_project, &topology.shepherd.projects)?
        {
            config.apply_project(project);
        }
        Ok(config)
    }
    fn apply_project(&mut self, project: &liberado_config::ShepherdProjectConfig) {
        self.repository = Some(project.repository.clone());
        self.check_names = project.check_names.clone();
        self.project = project.coding_project.clone();
        self.base = project.base_branch.clone();
        self.profile = project.profile.clone();
        self.max_kickbacks = project.max_kickbacks.unwrap_or(self.max_kickbacks);
        self.cold_reviews = project.cold_reviews.unwrap_or(self.cold_reviews);
        self.cold_turns = project.cold_review_max_turns.unwrap_or(self.cold_turns);
        self.max_concurrent = project.max_concurrent_goals.unwrap_or(self.max_concurrent);
        self.poll = project.poll_seconds.unwrap_or(self.poll);
    }
    fn state(&self) -> PathBuf {
        self.root.join(".liberado/shepherd")
    }
}

/// Resolve which shepherd project config applies — the three-way rule `Config::load` used to
/// inline: an explicit `--project <name>` wins and must exist; a single configured project
/// auto-applies; several without a name is an error rather than a guess; none means the
/// environment defaults.
fn select_shepherd_project<'a>(
    selected: Option<&str>,
    projects: &'a [liberado_config::ShepherdProjectConfig],
) -> Result<Option<&'a liberado_config::ShepherdProjectConfig>, String> {
    match selected {
        Some(name) => projects
            .iter()
            .find(|p| p.name == name)
            .map(Some)
            .ok_or_else(|| format!("unknown shepherd project '{name}'")),
        None if projects.len() == 1 => Ok(projects.first()),
        None if projects.len() > 1 => {
            Err("multiple shepherd projects configured; pass --project <name>".into())
        }
        None => Ok(None),
    }
}

fn load_shepherd_topology() -> Result<liberado_config::Topology, Box<dyn std::error::Error>> {
    let Some(dir) = liberado_config::config_dir() else {
        return Ok(liberado_config::Topology::default());
    };
    liberado_config_loader::ChainLoader::new()
        .add_source(Box::new(liberado_config_loader::FileSource::new(
            dir.join("topology.toml"),
        )))
        .load()
        .map_err(|error| format!("cannot load topology.toml: {error}").into())
}

/// Validate only the shepherd policy. Loading the full application configuration would make
/// forge observation depend on unrelated provider and webhook secrets.
fn validate_shepherd_topology(
    topology: &liberado_config::Topology,
) -> Result<(), Box<dyn std::error::Error>> {
    let declared_projects: BTreeSet<_> = topology
        .projects
        .iter()
        .map(|project| project.name.as_str())
        .collect();
    let mut names = BTreeSet::new();
    for project in &topology.shepherd.projects {
        if project.name.trim().is_empty() {
            return Err("shepherd project name must not be empty".into());
        }
        if !names.insert(project.name.as_str()) {
            return Err(format!("duplicate shepherd project name '{}'", project.name).into());
        }
        let mut repository = project.repository.split('/');
        if repository.next().is_none_or(str::is_empty)
            || repository.next().is_none_or(str::is_empty)
            || repository.next().is_some()
        {
            return Err(format!(
                "shepherd project '{}' repository must be OWNER/REPOSITORY",
                project.name
            )
            .into());
        }
        if !declared_projects.contains(project.coding_project.as_str()) {
            return Err(format!(
                "shepherd project '{}' references unknown coding_project '{}'",
                project.name, project.coding_project
            )
            .into());
        }
        if project.base_branch.trim().is_empty() || project.profile.trim().is_empty() {
            return Err(format!(
                "shepherd project '{}' base_branch and profile must not be empty",
                project.name
            )
            .into());
        }
        if project.max_concurrent_goals == Some(0) || project.poll_seconds == Some(0) {
            return Err(format!(
                "shepherd project '{}' max_concurrent_goals and poll_seconds must be greater than zero",
                project.name
            )
            .into());
        }
        let mut checks = BTreeSet::new();
        for check in &project.check_names {
            if check.trim().is_empty() || !checks.insert(check.as_str()) {
                return Err(format!(
                    "shepherd project '{}' check_names must be non-empty and unique",
                    project.name
                )
                .into());
            }
        }
    }
    Ok(())
}
#[derive(Clone)]
struct Pr {
    number: u64,
    title: String,
    branch: String,
    base_sha: String,
    labels: Vec<String>,
}
impl Pr {
    fn has(&self, label: &str) -> bool {
        self.labels.iter().any(|l| l == label)
    }
    fn count(&self, prefix: &str) -> usize {
        (1..10)
            .filter(|n| self.has(&format!("{prefix}{n}")))
            .count()
    }
    fn terminal(&self) -> bool {
        self.has(READY) || self.has(BLOCKED)
    }
}

/// One parsed shepherd invocation: which mode was asked for and with what modifiers.
///
/// Parsing is pure so the usage rules (a mode is required; `config` demands `check`; `--project`
/// takes the next argument) are testable without a repository or a daemon behind them.
#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    SelfTest,
    ConfigCheck { project: Option<String> },
    Drive { once: bool, watch: bool },
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedInvocation {
    mode: Invocation,
    dry_run: bool,
    project: Option<String>,
    seed: Option<PathBuf>,
    reset_baselines: bool,
}

fn parse_invocation(args: &[String]) -> Result<ParsedInvocation, String> {
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let project = args
        .windows(2)
        .find(|a| a[0] == "--project")
        .map(|a| a[1].clone());
    let seed = args
        .windows(2)
        .find(|a| a[0] == "--seed")
        .map(|a| PathBuf::from(&a[1]));
    let reset_baselines = args.iter().any(|a| a == "--reset-baselines");

    if args.iter().any(|a| a == "--self-test") {
        return Ok(ParsedInvocation {
            mode: Invocation::SelfTest,
            dry_run,
            project,
            seed,
            reset_baselines,
        });
    }
    if args.first().is_some_and(|arg| arg == "config") {
        if args.get(1).is_none_or(|arg| arg != "check") {
            return Err("usage: liberado shepherd config check [--project <name>]".into());
        }
        return Ok(ParsedInvocation {
            mode: Invocation::ConfigCheck {
                project: project.clone(),
            },
            dry_run,
            project,
            seed,
            reset_baselines,
        });
    }
    let once = args.iter().any(|a| a == "--once");
    let watch = args.iter().any(|a| a == "--watch");
    if !(once || watch || seed.is_some()) {
        return Err(
            "usage: liberado shepherd <--once|--watch|--seed FILE> [--project <name>] [--dry-run]\n       liberado shepherd config check [--project <name>]\n       liberado shepherd --self-test"
                .into(),
        );
    }
    Ok(ParsedInvocation {
        mode: Invocation::Drive { once, watch },
        dry_run,
        project,
        seed,
        reset_baselines,
    })
}

pub fn run(args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = args.collect();
    let parsed = parse_invocation(&args)?;
    match parsed.mode {
        Invocation::SelfTest => self_test(),
        Invocation::ConfigCheck { ref project } => config_check(project.as_deref()),
        Invocation::Drive { once, watch } => drive(&parsed, once, watch),
    }
}

fn drive(
    parsed: &ParsedInvocation,
    once: bool,
    watch: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let dry = parsed.dry_run;
    let cfg = Config::load(parsed.project.as_deref())?;
    if parsed.reset_baselines {
        reset_baselines(&cfg)?;
    }
    if let Some(path) = &parsed.seed {
        seed(&cfg, path, dry)?;
    }
    watch_loop(&cfg, once, watch, dry)
}

/// One pass over the open PRs: tick each, then count how many are still working.
fn tick_all(cfg: &Config, dry: bool) -> Result<(usize, usize), Box<dyn std::error::Error>> {
    let open_prs = prs(cfg)?;
    for mut pr in open_prs.clone() {
        if let Err(e) = tick(cfg, &mut pr, dry) {
            log(
                cfg,
                "tick_error",
                json!({"pr":pr.number,"detail":e.to_string()}),
            );
        }
    }
    let working = prs(cfg)?.into_iter().filter(|p| !p.terminal()).count();
    Ok((open_prs.len(), working))
}

/// The pass cadence: `--once` and plain `--seed` runs stop after one pass; `--watch` keeps
/// polling until nothing is left working.
fn watch_loop(
    cfg: &Config,
    once: bool,
    watch: bool,
    dry: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        let (open, working) = tick_all(cfg, dry)?;
        if once || !watch {
            log(
                cfg,
                "pass_complete",
                json!({"open_prs":open,"still_working":working}),
            );
            return Ok(());
        }
        if working == 0 {
            return Ok(());
        }
        thread::sleep(Duration::from_secs(cfg.poll));
    }
}
fn config_check(selected_project: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load(selected_project)?;
    println!("shepherd configuration:");
    println!(
        "  repository: {}",
        cfg.repository
            .as_deref()
            .unwrap_or("current checkout (legacy environment configuration)")
    );
    println!("  base branch: {}", cfg.base);
    println!("  coding project: {}", cfg.project);
    println!("  profile: {}", cfg.profile);
    println!(
        "  checks: {}",
        if cfg.check_names.is_empty() {
            "all reported checks".into()
        } else {
            cfg.check_names.join(", ")
        }
    );
    println!(
        "  limits: kickbacks={}, reviews={}, review_turns={}, concurrent={}, poll={}s",
        cfg.max_kickbacks, cfg.cold_reviews, cfg.cold_turns, cfg.max_concurrent, cfg.poll
    );
    Ok(())
}
fn gh(cfg: &Config, args: &[&str], check: bool) -> Result<String, Box<dyn std::error::Error>> {
    let mut command = std_command("gh");
    command.args(args).current_dir(&cfg.root);
    if let Some(repository) = &cfg.repository {
        command.args(["--repo", repository]);
    }
    let out = command.output()?;
    if check && !out.status.success() {
        return Err(format!(
            "gh {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into())
}
fn gh_json(cfg: &Config, args: &[&str]) -> Result<Value, Box<dyn std::error::Error>> {
    let output = gh(cfg, args, true)?;
    Ok(serde_json::from_str(&output)?)
}
fn log(cfg: &Config, event: &str, fields: Value) {
    let record = json!({"ts":Utc::now().to_rfc3339(),"event":event,"fields":fields});
    let _ = fs::create_dir_all(cfg.state());
    use std::io::Write;
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(cfg.state().join("events.jsonl"))
    {
        let _ = writeln!(f, "{record}");
    }
    println!(
        "[{}] {event}: {}",
        Utc::now().format("%H:%M:%S"),
        record["fields"]
    );
}

fn parse_failure_set(text: &str) -> BTreeSet<String> {
    let test = Regex::new(r"\btest\s+(\S+)\s+\.\.\.\s+FAILED\b").unwrap();
    let error = Regex::new(r"\b(error(\[[A-Z]\d+\])?:|error: could not compile)").unwrap();
    let (mut result, mut named, mut steps) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    for line in text.lines() {
        let parts: Vec<_> = line.split('\t').collect();
        if parts.len() < 3 {
            continue;
        }
        let key = format!("{}|{}", parts[0].trim(), parts[1].trim());
        if let Some(m) = test.captures(parts[2]) {
            result.insert(format!("{}|{}", parts[0].trim(), &m[1]));
            named.insert(key);
        } else if error.is_match(parts[2]) {
            steps.insert(key);
        }
    }
    for key in steps.difference(&named) {
        let (job, step) = key.split_once('|').unwrap();
        result.insert(format!("{job}|step:{step}"));
    }
    result
}
fn latest_run(
    cfg: &Config,
    branch: &str,
    sha: Option<&str>,
) -> Result<Option<Value>, Box<dyn std::error::Error>> {
    let rows = gh_json(
        cfg,
        &[
            "run",
            "list",
            "--branch",
            branch,
            "--limit",
            "20",
            "--json",
            "databaseId,headSha,status,conclusion,workflowName",
        ],
    )?;
    let Some(rows) = rows.as_array() else {
        return Ok(None);
    };
    Ok(rows
        .iter()
        .find(|r| {
            r["status"] == "completed"
                && sha.is_none_or(|wanted| {
                    r["headSha"]
                        .as_str()
                        .is_some_and(|s| s.starts_with(&wanted[..wanted.len().min(12)]))
                })
        })
        .cloned())
}
fn failure_set(cfg: &Config, id: u64) -> Result<BTreeSet<String>, Box<dyn std::error::Error>> {
    let id = id.to_string();
    let mut set = parse_failure_set(
        &gh(cfg, &["run", "view", &id, "--log-failed"], false).unwrap_or_default(),
    );
    let jobs = gh_json(cfg, &["run", "view", &id, "--json", "jobs"])?;
    for job in jobs["jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|j| j["conclusion"] == "failure")
    {
        let name = job["name"].as_str().unwrap_or("?");
        if set.iter().any(|k| k.starts_with(&format!("{name}|"))) {
            continue;
        }
        let mut found = false;
        for step in job["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| s["conclusion"] == "failure")
        {
            set.insert(format!(
                "{name}|step:{}",
                step["name"].as_str().unwrap_or("?")
            ));
            found = true;
        }
        if !found {
            set.insert(format!("{name}|step:<unknown step>"));
        }
    }
    Ok(set
        .into_iter()
        .filter(|key| check_selected(cfg, key))
        .collect())
}

fn check_selected(cfg: &Config, key: &str) -> bool {
    cfg.check_names.is_empty()
        || key
            .split_once('|')
            .is_some_and(|(job, _)| cfg.check_names.iter().any(|name| name == job))
}
fn baseline(
    cfg: &Config,
    sha: &str,
) -> Result<(BTreeSet<String>, String), Box<dyn std::error::Error>> {
    let dir = cfg.state().join("baselines");
    fs::create_dir_all(&dir)?;
    let short = &sha[..sha.len().min(12)];
    let path = dir.join(format!("{short}.json"));
    if let Ok(s) = fs::read_to_string(&path) {
        let v: Value = serde_json::from_str(&s)?;
        return Ok((
            v["failures"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            v["provenance"].as_str().unwrap_or("cache").into(),
        ));
    }
    let exact = latest_run(cfg, &cfg.base, Some(sha))?;
    let (run, provenance) = match exact {
        Some(r) => (Some(r), format!("exact:{short}")),
        None => {
            let r = latest_run(cfg, &cfg.base, None)?;
            let h = r
                .as_ref()
                .and_then(|r| r["headSha"].as_str())
                .unwrap_or("none")
                .to_owned();
            let provenance = format!("approx:{}", &h[..h.len().min(12)]);
            (r, provenance)
        }
    };
    let failures = run
        .as_ref()
        .and_then(|r| r["databaseId"].as_u64())
        .map(|id| failure_set(cfg, id))
        .transpose()?
        .unwrap_or_default();
    fs::write(
        path,
        serde_json::to_vec_pretty(
            &json!({"base_sha":sha,"failures":failures,"provenance":provenance,"computed_at":Utc::now().to_rfc3339()}),
        )?,
    )?;
    Ok((failures, provenance))
}

fn prs(cfg: &Config) -> Result<Vec<Pr>, Box<dyn std::error::Error>> {
    let response = gh_json(
        cfg,
        &[
            "pr",
            "list",
            "--state",
            "open",
            "--limit",
            "50",
            "--json",
            "number,title,headRefName,baseRefOid,labels,isDraft",
        ],
    )?;
    Ok(response
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| !r["isDraft"].as_bool().unwrap_or(false))
        .filter_map(|r| {
            Some(Pr {
                number: r["number"].as_u64()?,
                title: r["title"].as_str()?.into(),
                branch: r["headRefName"].as_str()?.into(),
                base_sha: r["baseRefOid"].as_str().unwrap_or("").into(),
                labels: r["labels"]
                    .as_array()?
                    .iter()
                    .filter_map(|v| v["name"].as_str().map(str::to_owned))
                    .collect(),
            })
        })
        .collect())
}
fn label(cfg: &Config, pr: &mut Pr, name: String) {
    let _ = gh(
        cfg,
        &["label", "create", &name, "--force", "--color", "ededed"],
        false,
    );
    let number = pr.number.to_string();
    let _ = gh(cfg, &["pr", "edit", &number, "--add-label", &name], false);
    pr.labels.push(name)
}
fn remove_label(cfg: &Config, pr: &mut Pr, name: &str) {
    let number = pr.number.to_string();
    let _ = gh(cfg, &["pr", "edit", &number, "--remove-label", name], false);
    pr.labels.retain(|l| l != name)
}
fn ci_status(cfg: &Config, pr: &Pr) -> Result<&'static str, Box<dyn std::error::Error>> {
    let number = pr.number.to_string();
    let value = gh_json(cfg, &["pr", "checks", &number, "--json", "state,name"])?;
    let Some(rows) = value.as_array() else {
        return Ok("none");
    };
    Ok(check_status(&cfg.check_names, rows))
}

fn check_status(check_names: &[String], rows: &[Value]) -> &'static str {
    if rows.is_empty() {
        return "none";
    }
    let names: BTreeSet<_> = rows.iter().filter_map(|row| row["name"].as_str()).collect();
    // A typo or renamed GitHub check must not silently mark a PR ready. Treat an absent selected
    // check as waiting, which is safe both before CI has reported and after a configuration error.
    if check_names
        .iter()
        .any(|expected| !names.contains(expected.as_str()))
    {
        return "pending";
    }
    let states: Vec<_> = rows
        .iter()
        .filter(|row| {
            check_names.is_empty()
                || row["name"]
                    .as_str()
                    .is_some_and(|name| check_names.iter().any(|expected| expected == name))
        })
        .filter_map(|r| r["state"].as_str())
        .map(|s| s.to_ascii_lowercase())
        .collect();
    if states
        .iter()
        .any(|s| matches!(s.as_str(), "pending" | "queued" | "in_progress" | ""))
    {
        "pending"
    } else if states
        .iter()
        .any(|s| matches!(s.as_str(), "failure" | "error" | "cancelled" | "timed_out"))
    {
        "failure"
    } else {
        "success"
    }
}
fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("HTTP client")
}
fn start_goal(cfg: &Config, description: String, max_turns: u32) -> Option<String> {
    let body = json!({"description":description,"domain":"coding","max_turns":max_turns,"profile":cfg.profile,"payload":{"project":cfg.project,"interactive":false}});
    let v: Value = client()
        .post(format!("{}/api/goals", cfg.daemon))
        .json(&body)
        .send()
        .ok()?
        .json()
        .ok()?;
    v["id"]
        .as_str()
        .or_else(|| v["session_id"].as_str())
        .map(str::to_owned)
}
fn active_goals(cfg: &Config) -> usize {
    let v: Value = match client()
        .get(format!("{}/api/goals", cfg.daemon))
        .send()
        .and_then(|r| r.json())
    {
        Ok(v) => v,
        Err(_) => return 0,
    };
    v.as_array()
        .into_iter()
        .flatten()
        .filter(|r| {
            matches!(
                r["status"]
                    .as_str()
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .as_str(),
                "running" | "pending" | "starting" | "active"
            )
        })
        .count()
}
fn goal_status(cfg: &Config, id: &str) -> Option<String> {
    let r = client()
        .get(format!("{}/api/goals/{id}", cfg.daemon))
        .send()
        .ok()?;
    if r.status().as_u16() == 404 {
        return Some("missing".into());
    }
    let v: Value = r.json().ok()?;
    parse_goal_status(&v)
}

fn parse_goal_status(value: &Value) -> Option<String> {
    value.get("session").unwrap_or(value)["status"]
        .as_str()
        .map(|s| s.to_ascii_lowercase())
}
fn pending(cfg: &Config, number: u64) -> PathBuf {
    cfg.state()
        .join("pending_reviews")
        .join(format!("{number}.json"))
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewTransition {
    None,
    Waiting,
    Labeled,
    Failed,
}

fn review_transition(status: Option<&str>) -> ReviewTransition {
    match status {
        None | Some("running" | "pending" | "starting" | "active" | "parked") => {
            ReviewTransition::Waiting
        }
        Some("succeeded") => ReviewTransition::Labeled,
        Some(_) => ReviewTransition::Failed,
    }
}

fn settle(
    cfg: &Config,
    pr: &mut Pr,
    dry: bool,
) -> Result<ReviewTransition, Box<dyn std::error::Error>> {
    settle_with(
        cfg,
        pr,
        dry,
        |id| goal_status(cfg, id),
        |pr, label_name| label(cfg, pr, label_name),
    )
}

fn settle_with(
    cfg: &Config,
    pr: &mut Pr,
    dry: bool,
    mut status_lookup: impl FnMut(&str) -> Option<String>,
    mut labeler: impl FnMut(&mut Pr, String),
) -> Result<ReviewTransition, Box<dyn std::error::Error>> {
    let path = pending(cfg, pr.number);
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(ReviewTransition::None);
    };
    let value: Value = serde_json::from_str(&text)?;
    let (Some(id), Some(round)) = (value["session_id"].as_str(), value["round"].as_u64()) else {
        if !dry {
            let _ = fs::remove_file(path);
        }
        return Ok(ReviewTransition::None);
    };
    match review_transition(status_lookup(id).as_deref()) {
        ReviewTransition::Waiting => Ok(ReviewTransition::Waiting),
        ReviewTransition::Labeled => {
            if !dry {
                labeler(pr, format!("shepherd:review-{round}"));
                let _ = fs::remove_file(path);
            }
            Ok(ReviewTransition::Labeled)
        }
        ReviewTransition::Failed => {
            if !dry {
                let _ = fs::remove_file(path);
            }
            Ok(ReviewTransition::Failed)
        }
        ReviewTransition::None => unreachable!("status transition is never none"),
    }
}
fn note(set: &BTreeSet<String>) -> String {
    if set.is_empty() {
        String::new()
    } else {
        format!(
            "{} failures were already on base; do not fix them:\n{}\n",
            set.len(),
            set.iter()
                .take(10)
                .map(|s| format!("  - {s}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}

fn kickback_prompt(pr: &Pr, failures: &BTreeSet<String>, old: &BTreeSet<String>) -> String {
    let list = failures
        .iter()
        .map(|failure| format!("  - {failure}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Pull request #{} (branch `{}`: {}) introduced {} new CI failure(s).\n\nNew failures:\n{}\n\n{}Do this:\n1. `git fetch origin` and check out `{}`.\n2. Reproduce a new failure locally before changing anything. A fix you never watched fail is a guess.\n3. Fix the cause. Do not delete, skip, or `#[ignore]` a test to get green. If a test is genuinely wrong, explain why in the commit message.\n4. Commit and push to `{}`.\n\nStay inside this scope. Do not refactor, reformat, or fix unrelated things.",
        pr.number,
        pr.branch,
        pr.title,
        failures.len(),
        list,
        note(old),
        pr.branch,
        pr.branch,
    )
}

fn cold_review_prompt(cfg: &Config, pr: &Pr, round: usize, old: &BTreeSet<String>) -> String {
    format!(
        "Cold review of pull request #{} (branch `{}`: {}). Round {} of {}.\n\nYou have no prior context on this change. Review it as written.\n\n1. `git fetch origin`, check out `{}`, and read `git diff origin/{}...HEAD`.\n2. Find real problems: bugs, missing edge cases, security holes, or broken invariants. Ignore style and formatting; CI already enforces those.\n3. For each suspicion, read the actual code and classify it as Real, Exaggerated, or Hallucinated. Fix only what is Real.\n4. For each real fix, add a test that fails without it and passes with it. Run it both ways; a test you never watched fail proves nothing.\n5. Commit and push to `{}`. If you found nothing Real, push nothing and say so.\n\n{}",
        pr.number,
        pr.branch,
        pr.title,
        round,
        cfg.cold_reviews,
        pr.branch,
        cfg.base,
        pr.branch,
        note(old),
    )
}

/// What a PR with fresh CI failures should get, decided from facts alone so tests can pin the
/// escalation ladder without `gh` or a daemon on the wire: rerun once, then kick back up to the
/// cap (a free slot required), then block.
#[derive(Debug, PartialEq, Eq)]
enum FailureAction {
    Rerun,
    Blocked,
    WaitForSlot,
    Kickback,
}

fn next_failure_action(
    has_rerun: bool,
    kicks: usize,
    max_kickbacks: usize,
    slot_free: impl FnOnce() -> bool,
) -> FailureAction {
    if !has_rerun {
        FailureAction::Rerun
    } else if kicks >= max_kickbacks {
        FailureAction::Blocked
    } else if !slot_free() {
        FailureAction::WaitForSlot
    } else {
        FailureAction::Kickback
    }
}

/// The clean-PR mirror of [`next_failure_action`]: ready once the cold-review cap is met,
/// otherwise spend a free slot on one more cold review.
#[derive(Debug, PartialEq, Eq)]
enum CleanAction {
    Ready,
    WaitForSlot,
    Review { round: usize },
}

fn next_clean_action(
    reviews: usize,
    cold_reviews: usize,
    slot_free: impl FnOnce() -> bool,
) -> CleanAction {
    if reviews >= cold_reviews {
        CleanAction::Ready
    } else if !slot_free() {
        CleanAction::WaitForSlot
    } else {
        CleanAction::Review { round: reviews + 1 }
    }
}

/// A PR with fresh CI failures: rerun once, then kick back a goal (up to the cap), then block.
///
/// Every arm ends the tick for this PR, so the caller does not fall through to the cold-review
/// path while failures are new.
fn handle_new_failures(
    cfg: &Config,
    pr: &mut Pr,
    dry: bool,
    new: &BTreeSet<String>,
    old: &BTreeSet<String>,
    run: &Option<Value>,
) -> Result<(), Box<dyn std::error::Error>> {
    let kicks = pr.count("shepherd:kickback-");
    let action = next_failure_action(pr.has(RERUN), kicks, cfg.max_kickbacks, || {
        active_goals(cfg) < cfg.max_concurrent
    });
    match action {
        FailureAction::Rerun => rerun_failed_run(cfg, pr, dry, run),
        FailureAction::Blocked => block_pr(cfg, pr, dry),
        // Waiting for a budget slot ends this PR's tick without side effects.
        FailureAction::WaitForSlot => Ok(()),
        FailureAction::Kickback => kickback(cfg, pr, dry, new, old, kicks),
    }
}

/// First sighting of fresh failures: rerun CI once before doing anything else.
fn rerun_failed_run(
    cfg: &Config,
    pr: &mut Pr,
    dry: bool,
    run: &Option<Value>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !dry && let Some(id) = run.as_ref().and_then(|r| r["databaseId"].as_u64()) {
        let id = id.to_string();
        let _ = gh(cfg, &["run", "rerun", &id, "--failed"], false);
        label(cfg, pr, RERUN.into())
    }
    Ok(())
}

fn block_pr(cfg: &Config, pr: &mut Pr, dry: bool) -> Result<(), Box<dyn std::error::Error>> {
    if !dry {
        label(cfg, pr, BLOCKED.into())
    }
    Ok(())
}

fn kickback(
    cfg: &Config,
    pr: &mut Pr,
    dry: bool,
    new: &BTreeSet<String>,
    old: &BTreeSet<String>,
    kicks: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if !dry {
        let prompt = kickback_prompt(pr, new, old);
        if let Some(id) = start_goal(cfg, prompt, 0) {
            label(cfg, pr, format!("shepherd:kickback-{}", kicks + 1));
            remove_label(cfg, pr, RERUN);
            log(
                cfg,
                "kickback_started",
                json!({"pr":pr.number,"session":id}),
            )
        }
    }
    Ok(())
}

/// A PR whose CI is now clean: ready it once the cold-review cap is met, otherwise spend a
/// budget slot on a cold review.
fn handle_clean(
    cfg: &Config,
    pr: &mut Pr,
    dry: bool,
    old: &BTreeSet<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let reviews = pr.count("shepherd:review-");
    let action = next_clean_action(reviews, cfg.cold_reviews, || {
        active_goals(cfg) < cfg.max_concurrent
    });
    match action {
        CleanAction::Ready => mark_ready(cfg, pr, dry),
        // Waiting for a budget slot ends this PR's tick without side effects.
        CleanAction::WaitForSlot => Ok(()),
        CleanAction::Review { round } => start_cold_review(cfg, pr, dry, old, round),
    }
}

fn mark_ready(cfg: &Config, pr: &mut Pr, dry: bool) -> Result<(), Box<dyn std::error::Error>> {
    if !dry {
        label(cfg, pr, READY.into())
    }
    Ok(())
}

/// Spend a budget slot on one cold review and record the pending round so `settle` can find it.
fn start_cold_review(
    cfg: &Config,
    pr: &Pr,
    dry: bool,
    old: &BTreeSet<String>,
    round: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if !dry {
        let prompt = cold_review_prompt(cfg, pr, round, old);
        if let Some(id) = start_goal(cfg, prompt, cfg.cold_turns) {
            let path = pending(cfg, pr.number);
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(
                path,
                serde_json::to_vec(&json!({"session_id":id,"round":round}))?,
            )?
        }
    }
    Ok(())
}

/// Pure gate: whether the CI signal is too weak for `tick` to act. `pending` and `none` both mean
/// the checks have not produced a settled outcome, so `tick` returns without touching the PR. Kept
/// separate from `settle`, which has side effects (it may wait or label) and must not run on a nil
/// signal. The terminal-PR short-circuit stays inline in `tick` because it also guards the
/// `ci_status` network call below.
fn tick_idle(status: &str) -> bool {
    status == "pending" || status == "none"
}

/// The failure sets [`ci_delta`] splits a PR's CI signal into.
type CiDelta = (BTreeSet<String>, BTreeSet<String>, Option<Value>);

/// The current failure set of the PR's latest run, split against the baseline of its base
/// commit: everything not in the baseline is *new* (this push), everything shared is
/// *pre-existing*. Also logs the delta — the split decision is only ever made with the counts
/// recorded next to it.
fn ci_delta(cfg: &Config, pr: &Pr) -> Result<CiDelta, Box<dyn std::error::Error>> {
    let run = latest_run(cfg, &pr.branch, None)?;
    let current = run
        .as_ref()
        .and_then(|r| r["databaseId"].as_u64())
        .map(|id| failure_set(cfg, id))
        .transpose()?
        .unwrap_or_default();
    let (base, provenance) = if pr.base_sha.is_empty() {
        (BTreeSet::new(), "no-base".to_string())
    } else {
        baseline(cfg, &pr.base_sha)?
    };
    let new: BTreeSet<_> = current.difference(&base).cloned().collect();
    let old: BTreeSet<_> = current.intersection(&base).cloned().collect();
    log(
        cfg,
        "ci_delta",
        json!({"pr":pr.number,"new":new.len(),"preexisting":old.len(),"base":provenance}),
    );
    log(
        cfg,
        "ci_delta",
        json!({"pr":pr.number,"new":new.len(),"preexisting":old.len(),"base":provenance}),
    );
    Ok((new, old, run))
}

fn tick(cfg: &Config, pr: &mut Pr, dry: bool) -> Result<(), Box<dyn std::error::Error>> {
    if pr.terminal() {
        return Ok(());
    }
    if tick_idle(ci_status(cfg, pr)?) {
        return Ok(());
    }
    if settle(cfg, pr, dry)? == ReviewTransition::Waiting {
        return Ok(());
    }
    let (new, old, run) = ci_delta(cfg, pr)?;
    if !new.is_empty() {
        return handle_new_failures(cfg, pr, dry, &new, &old, &run);
    }
    handle_clean(cfg, pr, dry, &old)
}

fn seed(cfg: &Config, path: &Path, dry: bool) -> Result<(), Box<dyn std::error::Error>> {
    for task in fs::read_to_string(path)?
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
    {
        while !dry && active_goals(cfg) >= cfg.max_concurrent {
            thread::sleep(Duration::from_secs(cfg.poll))
        }
        if !dry {
            let _ = start_goal(cfg, task.into(), 0);
        }
    }
    Ok(())
}
fn reset_baselines(cfg: &Config) -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(entries) = fs::read_dir(cfg.state().join("baselines")) {
        for entry in entries.flatten() {
            if entry.path().extension().is_some_and(|e| e == "json") {
                fs::remove_file(entry.path())?
            }
        }
    }
    Ok(())
}
fn self_test() -> Result<(), Box<dyn std::error::Error>> {
    let got = parse_failure_set(
        "test (ubuntu-latest)\tTests\tX test a::b ... FAILED\ntest (ubuntu-latest)\tTests\tX test c::d ... FAILED\ntest (windows-latest)\tTests\tX test a::b ... FAILED\nclippy\tLint\tX error: could not compile `x`",
    );
    assert_eq!(got.len(), 4);
    assert!(got.contains("clippy|step:Lint"));
    assert!(got.contains("test (ubuntu-latest)|a::b"));
    assert!(got.contains("test (windows-latest)|a::b"));
    let base: BTreeSet<String> = BTreeSet::from(["j|a".into(), "j|b".into()]);
    let head: BTreeSet<String> = BTreeSet::from(["j|a".into(), "j|c".into()]);
    assert_eq!(base.len(), head.len());
    assert!(head.difference(&base).next().is_some());
    assert_eq!(
        parse_goal_status(&json!({"session":{"status":"Succeeded"}})),
        Some("succeeded".into())
    );
    assert_eq!(review_transition(Some("failed")), ReviewTransition::Failed);
    assert_eq!(
        review_transition(Some("running")),
        ReviewTransition::Waiting
    );
    println!("shepherd self-test: ok");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(root: PathBuf) -> Config {
        Config {
            root,
            repository: None,
            check_names: Vec::new(),
            daemon: String::new(),
            project: String::new(),
            base: "main".into(),
            profile: String::new(),
            max_kickbacks: 2,
            cold_reviews: 2,
            cold_turns: 60,
            max_concurrent: 2,
            poll: 120,
        }
    }
    #[test]
    fn parser_is_platform_specific_and_preserves_step_failure() {
        self_test().unwrap()
    }
    #[test]
    fn preexisting_note_is_bounded() {
        let set = (0..11).map(|i| format!("j|{i}")).collect();
        assert!(note(&set).lines().count() <= 12)
    }

    #[test]
    fn selected_checks_filter_by_job_name() {
        let cfg = Config {
            root: PathBuf::new(),
            repository: None,
            check_names: vec!["test (windows-latest)".into()],
            daemon: String::new(),
            project: String::new(),
            base: String::new(),
            profile: String::new(),
            max_kickbacks: 0,
            cold_reviews: 0,
            cold_turns: 0,
            max_concurrent: 0,
            poll: 0,
        };
        assert!(check_selected(&cfg, "test (windows-latest)|crate::test"));
        assert!(!check_selected(&cfg, "test (ubuntu-latest)|crate::test"));
    }

    #[test]
    fn missing_selected_check_is_not_success() {
        let rows = vec![json!({"name":"test (ubuntu)","state":"SUCCESS"})];
        assert_eq!(check_status(&["test (windows)".into()], &rows), "pending");
    }

    #[test]
    fn shepherd_config_rejects_unknown_coding_project() {
        let mut topology = liberado_config::Topology::default();
        topology
            .shepherd
            .projects
            .push(liberado_config::ShepherdProjectConfig {
                name: "example".into(),
                repository: "owner/repo".into(),
                coding_project: "missing".into(),
                base_branch: "main".into(),
                profile: "coding-unattended".into(),
                check_names: Vec::new(),
                max_kickbacks: None,
                cold_reviews: None,
                cold_review_max_turns: None,
                max_concurrent_goals: None,
                poll_seconds: None,
            });
        assert!(
            validate_shepherd_topology(&topology)
                .unwrap_err()
                .to_string()
                .contains("unknown coding_project")
        );
    }

    /// A minimal valid shepherd project, for the validation-branch tests below.
    fn valid_project(name: &str) -> liberado_config::ShepherdProjectConfig {
        liberado_config::ShepherdProjectConfig {
            name: name.into(),
            repository: "owner/repo".into(),
            coding_project: "liberado".into(),
            base_branch: "main".into(),
            profile: "coding-unattended".into(),
            check_names: vec!["test".into()],
            max_kickbacks: None,
            cold_reviews: None,
            cold_review_max_turns: None,
            max_concurrent_goals: None,
            poll_seconds: None,
        }
    }

    fn topology_with(project: liberado_config::ShepherdProjectConfig) -> liberado_config::Topology {
        let mut topology = liberado_config::Topology::default();
        // The project must be declared in the application `[projects]` list too, or validation
        // fails on the unknown-coding-project check before reaching the branch under test.
        topology.projects.push(liberado_config::ProjectConfig {
            name: project.coding_project.clone(),
            root: PathBuf::from("/tmp/project"),
            write_class: liberado_common::WriteClass::AgentWritable,
            enabled: true,
            preflight: Default::default(),
        });
        topology.shepherd.projects.push(project);
        topology
    }

    fn rejects(project: liberado_config::ShepherdProjectConfig, needle: &str) {
        let error = validate_shepherd_topology(&topology_with(project))
            .unwrap_err()
            .to_string();
        assert!(error.contains(needle), "expected {needle:?} in {error:?}");
    }

    /// Every invalid shape is refused with a message that names the problem — a shepherd that
    /// silently accepts a broken topology would mislabel PRs instead.
    #[test]
    fn shepherd_config_rejects_every_invalid_shape() {
        rejects(valid_project(""), "name must not be empty");
        rejects(
            liberado_config::ShepherdProjectConfig {
                name: "dupe".into(),
                repository: "not-owner-repo".into(),
                ..valid_project("dupe")
            },
            "OWNER/REPOSITORY",
        );
        rejects(
            liberado_config::ShepherdProjectConfig {
                base_branch: "  ".into(),
                ..valid_project("blank-base")
            },
            "base_branch and profile must not be empty",
        );
        rejects(
            liberado_config::ShepherdProjectConfig {
                max_concurrent_goals: Some(0),
                ..valid_project("zero-concurrent")
            },
            "must be greater than zero",
        );
        rejects(
            liberado_config::ShepherdProjectConfig {
                poll_seconds: Some(0),
                ..valid_project("zero-poll")
            },
            "must be greater than zero",
        );
        rejects(
            liberado_config::ShepherdProjectConfig {
                check_names: vec![String::new()],
                ..valid_project("empty-check")
            },
            "non-empty and unique",
        );
    }

    #[test]
    fn shepherd_config_rejects_duplicate_project_names() {
        let mut topology = topology_with(valid_project("dupe"));
        topology.shepherd.projects.push(valid_project("dupe"));
        let error = validate_shepherd_topology(&topology)
            .unwrap_err()
            .to_string();
        assert!(error.contains("duplicate shepherd project name"), "{error}");
    }

    #[test]
    fn shepherd_config_accepts_a_valid_project() {
        assert!(validate_shepherd_topology(&topology_with(valid_project("ok"))).is_ok());
    }

    // ── select_shepherd_project ────────────────────────────────────────

    #[test]
    fn project_selection_follows_the_three_way_rule() {
        let one = valid_project("one");
        let two = valid_project("two");
        // An explicit name wins and must exist.
        let both = [one.clone(), two.clone()];
        let picked = select_shepherd_project(Some("two"), &both)
            .unwrap()
            .unwrap();
        assert_eq!(picked.name, "two");
        assert!(
            select_shepherd_project(Some("nope"), std::slice::from_ref(&one))
                .unwrap_err()
                .contains("unknown shepherd project")
        );
        // A single configured project auto-applies.
        let single = [one.clone()];
        let picked = select_shepherd_project(None, &single).unwrap().unwrap();
        assert_eq!(picked.name, "one");
        // Several without a name is an error, not a guess.
        let pair = [one, two];
        assert!(
            select_shepherd_project(None, &pair)
                .unwrap_err()
                .contains("multiple")
        );
        // None configured means the environment defaults apply.
        assert!(select_shepherd_project(None, &[]).unwrap().is_none());
    }

    // ── apply_project / state ───────────────────────────────────────────

    #[test]
    fn apply_project_copies_every_field() {
        let mut cfg = test_config(PathBuf::from("/tmp/root"));
        let project = liberado_config::ShepherdProjectConfig {
            name: "p".into(),
            repository: "owner/repo".into(),
            coding_project: "proj".into(),
            base_branch: "dev".into(),
            profile: "prof".into(),
            check_names: vec!["a".into(), "b".into()],
            max_kickbacks: Some(1),
            cold_reviews: Some(3),
            cold_review_max_turns: Some(9),
            max_concurrent_goals: Some(4),
            poll_seconds: Some(30),
        };
        cfg.apply_project(&project);
        assert_eq!(cfg.repository.as_deref(), Some("owner/repo"));
        assert_eq!(cfg.check_names, vec!["a", "b"]);
        assert_eq!(cfg.project, "proj");
        assert_eq!(cfg.base, "dev");
        assert_eq!(cfg.profile, "prof");
        assert_eq!(cfg.max_kickbacks, 1);
        assert_eq!(cfg.cold_reviews, 3);
        assert_eq!(cfg.cold_turns, 9);
        assert_eq!(cfg.max_concurrent, 4);
        assert_eq!(cfg.poll, 30);
    }

    #[test]
    fn state_lives_under_the_shepherd_dir() {
        let cfg = test_config(PathBuf::from("/tmp/root"));
        assert_eq!(cfg.state(), PathBuf::from("/tmp/root/.liberado/shepherd"));
    }

    // ── reset_baselines ─────────────────────────────────────────────────

    #[test]
    fn reset_baselines_removes_only_json_caches() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = test_config(temp.path().to_path_buf());
        let dir = cfg.state().join("baselines");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("abc.json"), "{}").unwrap();
        fs::write(dir.join("keep.txt"), "x").unwrap();
        reset_baselines(&cfg).unwrap();
        assert!(!dir.join("abc.json").exists(), "json cache must be removed");
        assert!(
            dir.join("keep.txt").exists(),
            "non-json files are not baselines"
        );
    }

    // ── Config::get ─────────────────────────────────────────────────────

    /// `get` reads its environment variable, falling back to the default when unset. The var name
    /// is unique to this test so the set/clear pair cannot be observed by another test.
    #[test]
    fn config_get_reads_env_with_a_default() {
        let key = "SHEPHERD_TEST_GET_9f2c7d";
        // Edition 2024 marks these unsafe: the pair is scoped to this test with a unique key.
        unsafe { std::env::set_var(key, "from-env") };
        assert_eq!(Config::get(key, "fallback"), "from-env");
        unsafe { std::env::remove_var(key) };
        assert_eq!(Config::get(key, "fallback"), "fallback");
    }

    // ── seed (dry mode) ────────────────────────────────────────────────

    /// Dry mode parses and validates the task file but never talks to the daemon.
    #[test]
    fn seed_in_dry_mode_parses_tasks_without_starting_goals() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = test_config(temp.path().to_path_buf());
        let task = temp.path().join("tasks.txt");
        fs::write(&task, "# comment\n\nfirst task\nsecond task\n\n").unwrap();
        assert!(seed(&cfg, &task, true).is_ok());
        // A missing file is still an error in dry mode.
        assert!(seed(&cfg, &temp.path().join("nope.txt"), true).is_err());
    }

    #[test]
    fn tick_idle_gates_pending_and_none_but_not_settled() {
        assert!(tick_idle("pending"), "pending CI is not a settled signal");
        assert!(tick_idle("none"), "no CI run is not a settled signal");
        assert!(!tick_idle("completed"), "settled CI lets tick proceed");
    }

    /// The baseline cache is read without touching the network: a `baselines/<short>.json` file
    /// written by a previous run is the whole answer.
    #[test]
    fn baseline_reads_the_cached_failure_set() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = test_config(temp.path().to_path_buf());
        let sha = "0123456789abcdef";
        let dir = cfg.state().join("baselines");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("{}.json", &sha[..12])),
            serde_json::to_vec(&json!({
                "base_sha": sha,
                "failures": ["job|test::a", "job|step:Lint"],
                "provenance": "cache",
            }))
            .unwrap(),
        )
        .unwrap();
        let (failures, provenance) = baseline(&cfg, sha).unwrap();
        assert_eq!(provenance, "cache");
        assert!(failures.contains("job|test::a"), "{failures:?}");
        assert!(failures.contains("job|step:Lint"), "{failures:?}");
    }

    #[test]
    fn settled_review_labels_only_on_success_and_preserves_dry_run_state() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = test_config(temp.path().to_path_buf());
        let mut pr = Pr {
            number: 42,
            title: "test".into(),
            branch: "test".into(),
            base_sha: String::new(),
            labels: Vec::new(),
        };
        let path = pending(&cfg, pr.number);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"session_id":"one","round":1}"#).unwrap();
        let mut labels = Vec::new();
        assert_eq!(
            settle_with(
                &cfg,
                &mut pr,
                false,
                |_| Some("failed".into()),
                |_, label| labels.push(label)
            )
            .unwrap(),
            ReviewTransition::Failed
        );
        assert!(!path.exists());
        assert!(labels.is_empty());

        fs::write(&path, r#"{"session_id":"two","round":1}"#).unwrap();
        assert_eq!(
            settle_with(
                &cfg,
                &mut pr,
                false,
                |_| Some("succeeded".into()),
                |_, label| labels.push(label)
            )
            .unwrap(),
            ReviewTransition::Labeled
        );
        assert!(!path.exists());
        assert_eq!(labels, ["shepherd:review-1"]);

        fs::write(&path, r#"{"session_id":"three","round":2}"#).unwrap();
        assert_eq!(
            settle_with(
                &cfg,
                &mut pr,
                true,
                |_| Some("succeeded".into()),
                |_, label| labels.push(label)
            )
            .unwrap(),
            ReviewTransition::Labeled
        );
        assert!(path.exists());
        assert_eq!(labels, ["shepherd:review-1"]);
    }

    #[test]
    fn prompts_keep_unattended_guardrails() {
        let cfg = test_config(PathBuf::new());
        let pr = Pr {
            number: 1,
            title: "title".into(),
            branch: "branch".into(),
            base_sha: String::new(),
            labels: Vec::new(),
        };
        let failures = BTreeSet::from(["test|case".into()]);
        let kickback = kickback_prompt(&pr, &failures, &BTreeSet::new());
        assert!(kickback.contains("Reproduce a new failure locally before changing anything"));
        assert!(kickback.contains("Do not delete, skip, or `#[ignore]` a test"));
        let review = cold_review_prompt(&cfg, &pr, 1, &BTreeSet::new());
        assert!(review.contains("Real, Exaggerated, or Hallucinated"));
        assert!(review.contains("Run it both ways"));
    }

    // ── parse_failure_set ───────────────────────────────────────────────

    /// A rustc error with a diagnostic code (`error[E0123]`) is a step failure, folded into the
    /// result as `job|step:<step>` — the self-test's plain `error:` is only one spelling.
    #[test]
    fn failure_set_recognises_diagnostic_codes() {
        let set =
            parse_failure_set("test (ubuntu-latest)\tLint\terror[E0123]: unresolved import\n");
        assert!(set.contains("test (ubuntu-latest)|step:Lint"), "{set:?}");
    }

    /// `error: could not compile` (the old cargo spelling, no bracket code) is a step failure too.
    #[test]
    fn failure_set_recognises_could_not_compile() {
        let set = parse_failure_set(
            "clippy\tLint\terror: could not compile `x` (due to 3 previous errors)\n",
        );
        assert!(set.contains("clippy|step:Lint"), "{set:?}");
    }

    /// A step whose named test failed must not ALSO be reported as a bare step failure — that
    /// would double-count one failure.
    #[test]
    fn failure_set_does_not_double_count_named_tests() {
        let set = parse_failure_set("test (ubuntu-latest)\tTests\tX test crate::case ... FAILED\n");
        assert!(set.contains("test (ubuntu-latest)|crate::case"), "{set:?}");
        assert!(!set.contains("test (ubuntu-latest)|step:Tests"), "{set:?}");
    }

    /// Malformed rows (fewer than the three tab-separated columns) are skipped, not fatal.
    #[test]
    fn failure_set_skips_short_rows() {
        let set = parse_failure_set("only-two-columns\tignored\ngarbage\n");
        assert!(set.is_empty(), "{set:?}");
    }

    // ── check_status ────────────────────────────────────────────────────

    fn row(name: &str, state: &str) -> serde_json::Value {
        json!({ "name": name, "state": state })
    }

    /// No checks reported yet reads as "none" — the PR is neither green nor red, just unreported.
    #[test]
    fn check_status_none_when_no_rows() {
        assert_eq!(check_status(&[], &[]), "none");
    }

    /// All passing → success; any pending/queued/in-progress state → pending (never success
    /// early); any failure/error state → failure.
    #[test]
    fn check_status_aggregates_states() {
        let rows = vec![row("a", "SUCCESS"), row("b", "success")];
        assert_eq!(check_status(&[], &rows), "success");
        let rows = vec![row("a", "SUCCESS"), row("b", "in_progress")];
        assert_eq!(check_status(&[], &rows), "pending");
        let rows = vec![row("a", "SUCCESS"), row("b", "failure")];
        assert_eq!(check_status(&[], &rows), "failure");
        let rows = vec![row("a", "queued"), row("b", "timed_out")];
        assert_eq!(check_status(&[], &rows), "pending");
    }

    /// An empty check-name filter means "all reported checks" — the full row set is the gate.
    #[test]
    fn check_status_with_no_filter_uses_all_rows() {
        let rows = vec![row("a", "SUCCESS")];
        assert_eq!(check_status(&[], &rows), "success");
    }

    // ── Pr ──────────────────────────────────────────────────────────────

    fn pr(labels: &[&str]) -> Pr {
        Pr {
            number: 7,
            title: "t".into(),
            branch: "b".into(),
            base_sha: String::new(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// `has` is exact-label matching; `count` counts the numbered kickback labels; `terminal` is
    /// ready-or-blocked.
    #[test]
    fn pr_label_helpers() {
        let p = pr(&["shepherd:kickback-1", "shepherd:kickback-2", READY]);
        assert!(p.has("shepherd:kickback-1"));
        assert!(!p.has("shepherd:kickback-3"));
        assert_eq!(p.count("shepherd:kickback-"), 2);
        assert!(p.terminal());
        assert!(!pr(&["shepherd:kickback-1"]).terminal());
        assert!(pr(&[BLOCKED]).terminal());
    }

    // ── review_transition ───────────────────────────────────────────────

    /// Every status the daemon reports maps to exactly one transition; an unknown status fails
    /// closed (the review is treated as failed rather than silently passing).
    #[test]
    fn review_transition_covers_every_status() {
        for waiting in [
            None,
            Some("running"),
            Some("pending"),
            Some("starting"),
            Some("active"),
            Some("parked"),
        ] {
            assert_eq!(
                review_transition(waiting),
                ReviewTransition::Waiting,
                "{waiting:?}"
            );
        }
        assert_eq!(
            review_transition(Some("succeeded")),
            ReviewTransition::Labeled
        );
        for failed in [Some("failed"), Some("cancelled"), Some("lost")] {
            assert_eq!(
                review_transition(failed),
                ReviewTransition::Failed,
                "{failed:?}"
            );
        }
    }

    // ── note ────────────────────────────────────────────────────────────

    #[test]
    fn note_is_empty_for_no_preexisting_failures() {
        assert_eq!(note(&BTreeSet::new()), "");
    }

    // ── parse_goal_status ───────────────────────────────────────────────

    /// The status lives either at the top level or under `session`, lowercase either way. (A bare
    /// status string is not a shape the daemon emits.)
    #[test]
    fn goal_status_reads_top_level_or_session() {
        assert_eq!(
            parse_goal_status(&json!({"status": "Running"})),
            Some("running".into())
        );
        assert_eq!(
            parse_goal_status(&json!({"session": {"status": "succeeded"}})),
            Some("succeeded".into())
        );
        assert_eq!(parse_goal_status(&json!({})), None);
        assert_eq!(parse_goal_status(&json!("running")), None);
    }

    #[test]
    fn limits_default_when_the_environment_says_nothing() {
        let get = |_key: &str| None;
        let limits = Limits::from_reader(get).unwrap();
        assert_eq!(
            limits,
            Limits {
                max_kickbacks: 2,
                cold_reviews: 2,
                cold_turns: 60,
                max_concurrent: 2,
                poll: 120,
            }
        );
    }

    #[test]
    fn limits_read_valid_overrides() {
        let get = |key: &str| match key {
            "SHEPHERD_MAX_KICKBACKS" => Some("5".into()),
            "SHEPHERD_POLL_SECONDS" => Some("30".into()),
            _ => None,
        };
        let limits = Limits::from_reader(get).unwrap();
        assert_eq!(limits.max_kickbacks, 5);
        assert_eq!(limits.poll, 30);
        assert_eq!(limits.cold_reviews, 2, "unset keys keep their default");
    }

    #[test]
    fn limits_name_the_key_of_a_malformed_value() {
        let get = |key: &str| match key {
            "SHEPHERD_COLD_REVIEWS" => Some("many".into()),
            _ => None,
        };
        let error = Limits::from_reader(get).unwrap_err();
        assert!(
            error.contains("SHEPHERD_COLD_REVIEWS") && error.contains("many"),
            "the error must name the key and the bad value: {error}"
        );
    }

    #[test]
    fn failure_escalation_reruns_once_then_kicks_then_blocks() {
        // First sighting: rerun CI before doing anything else.
        assert_eq!(
            next_failure_action(false, 0, 2, || true),
            FailureAction::Rerun
        );
        // A rerun already spent and the kickback cap reached: blocked, no matter the slots.
        assert_eq!(
            next_failure_action(true, 2, 2, || true),
            FailureAction::Blocked
        );
        // Cap not reached but every budget slot busy: wait.
        assert_eq!(
            next_failure_action(true, 0, 2, || false),
            FailureAction::WaitForSlot
        );
        // Rerun spent, cap not reached, slot free: start a kickback goal.
        assert_eq!(
            next_failure_action(true, 1, 2, || true),
            FailureAction::Kickback
        );
    }

    #[test]
    fn clean_prs_ready_after_cap_else_review_when_a_slot_frees_up() {
        assert_eq!(next_clean_action(2, 2, || true), CleanAction::Ready);
        assert_eq!(next_clean_action(1, 3, || false), CleanAction::WaitForSlot);
        assert_eq!(
            next_clean_action(1, 3, || true),
            CleanAction::Review { round: 2 },
            "the review round continues from the reviews already done"
        );
    }

    #[test]
    fn apply_project_overrides_only_the_fields_it_declares() {
        let mut cfg = test_config(PathBuf::new());
        cfg.daemon = "http://env-daemon".into();
        let project = liberado_config::ShepherdProjectConfig {
            name: "p".into(),
            repository: "owner/repo".into(),
            coding_project: "proj".into(),
            base_branch: "trunk".into(),
            profile: "coding".into(),
            check_names: vec!["test".into()],
            max_kickbacks: Some(7),
            cold_reviews: None,
            cold_review_max_turns: None,
            max_concurrent_goals: None,
            poll_seconds: None,
        };
        cfg.apply_project(&project);
        assert_eq!(cfg.repository.as_deref(), Some("owner/repo"));
        assert_eq!(cfg.project, "proj");
        assert_eq!(cfg.base, "trunk");
        assert_eq!(cfg.profile, "coding");
        assert_eq!(cfg.max_kickbacks, 7);
        assert_eq!(cfg.check_names, vec!["test".to_string()]);
        // Fields the project leaves unset keep their environment-derived values.
        assert_eq!(cfg.cold_reviews, 2);
        assert_eq!(cfg.cold_turns, 60);
        assert_eq!(cfg.max_concurrent, 2);
        assert_eq!(cfg.poll, 120);
        assert_eq!(
            cfg.daemon, "http://env-daemon",
            "daemon is never project-set"
        );
    }

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn invocation_parses_every_documented_shape() {
        assert!(matches!(
            parse_invocation(&argv(&["--self-test"])).unwrap().mode,
            Invocation::SelfTest
        ));
        let config_check =
            parse_invocation(&argv(&["config", "check", "--project", "liberado"])).unwrap();
        assert_eq!(
            config_check.mode,
            Invocation::ConfigCheck {
                project: Some("liberado".into())
            }
        );
        let seed_drive = parse_invocation(&argv(&["--seed", "tasks.txt", "--dry-run"])).unwrap();
        assert_eq!(
            seed_drive.mode,
            Invocation::Drive {
                once: false,
                watch: false
            }
        );
        assert_eq!(seed_drive.seed, Some(PathBuf::from("tasks.txt")));
        assert!(seed_drive.dry_run);
    }

    #[test]
    fn invocation_demands_a_mode_before_doing_anything() {
        let error = parse_invocation(&argv(&["--dry-run"])).unwrap_err();
        assert!(error.contains("--once|--watch|--seed"), "{error}");
    }

    #[test]
    fn invocation_rejects_config_without_check() {
        let error = parse_invocation(&argv(&["config"])).unwrap_err();
        assert!(error.contains("config check"), "{error}");
    }

    #[test]
    fn invocation_carries_the_secondary_flags() {
        let parsed = parse_invocation(&argv(&[
            "--once",
            "--reset-baselines",
            "--project",
            "other",
        ]))
        .unwrap();
        assert!(parsed.reset_baselines);
        assert!(!parsed.dry_run);
        assert_eq!(parsed.project.as_deref(), Some("other"));
        assert_eq!(
            parsed.mode,
            Invocation::Drive {
                once: true,
                watch: false
            }
        );
    }
}
