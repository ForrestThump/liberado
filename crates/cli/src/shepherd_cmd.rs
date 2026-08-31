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
    handle_settled_tick(cfg, pr, dry)
}

fn handle_settled_tick(
    cfg: &Config,
    pr: &mut Pr,
    dry: bool,
) -> Result<(), Box<dyn std::error::Error>> {
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
#[path = "shepherd_cmd_tests.rs"]
mod shepherd_cmd_tests;
