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
    fn number<T: std::str::FromStr>(key: &str, default: T) -> Result<T, Box<dyn std::error::Error>>
    where
        T::Err: std::error::Error + 'static,
    {
        Ok(std::env::var(key).ok().map_or(Ok(default), |v| v.parse())?)
    }
    fn load(selected_project: Option<&str>) -> Result<Self, Box<dyn std::error::Error>> {
        let mut config = Self {
            root: crate::crate_map_cmd::repository_root()?,
            repository: None,
            check_names: Vec::new(),
            daemon: Self::get("LIBERADO_SERVER", "http://localhost:4201"),
            project: Self::get("SHEPHERD_PROJECT", "liberado"),
            base: Self::get("SHEPHERD_BASE", "main"),
            profile: Self::get("SHEPHERD_PROFILE", "coding-unattended"),
            max_kickbacks: Self::number("SHEPHERD_MAX_KICKBACKS", 2)?,
            cold_reviews: Self::number("SHEPHERD_COLD_REVIEWS", 2)?,
            cold_turns: Self::number("SHEPHERD_COLD_REVIEW_MAX_TURNS", 60)?,
            max_concurrent: Self::number("SHEPHERD_MAX_CONCURRENT", 2)?,
            poll: Self::number("SHEPHERD_POLL_SECONDS", 120)?,
        };
        let topology = load_shepherd_topology()?;
        validate_shepherd_topology(&topology)?;
        match (selected_project, topology) {
            (Some(name), topology) => {
                let project = topology
                    .shepherd
                    .projects
                    .iter()
                    .find(|project| project.name == name)
                    .ok_or_else(|| format!("unknown shepherd project '{name}'"))?;
                config.apply_project(project);
            }
            (None, topology) if topology.shepherd.projects.len() == 1 => {
                config.apply_project(&topology.shepherd.projects[0]);
            }
            (None, topology) if topology.shepherd.projects.len() > 1 => {
                return Err("multiple shepherd projects configured; pass --project <name>".into());
            }
            (None, _) => {}
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

pub fn run(args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = args.collect();
    if args.iter().any(|a| a == "--self-test") {
        return self_test();
    }
    let once = args.iter().any(|a| a == "--once");
    let watch = args.iter().any(|a| a == "--watch");
    let dry = args.iter().any(|a| a == "--dry-run");
    let selected_project = args
        .windows(2)
        .find(|a| a[0] == "--project")
        .map(|a| a[1].as_str());
    if args.first().is_some_and(|arg| arg == "config") {
        if args.get(1).is_none_or(|arg| arg != "check") {
            return Err("usage: liberado shepherd config check [--project <name>]".into());
        }
        return config_check(selected_project);
    }
    let seed_path = args
        .windows(2)
        .find(|a| a[0] == "--seed")
        .map(|a| PathBuf::from(&a[1]));
    if !(once || watch || seed_path.is_some()) {
        return Err(
            "usage: liberado shepherd <--once|--watch|--seed FILE> [--project <name>] [--dry-run]\n       liberado shepherd config check [--project <name>]\n       liberado shepherd --self-test"
                .into(),
        );
    }
    let cfg = Config::load(selected_project)?;
    if args.iter().any(|a| a == "--reset-baselines") {
        reset_baselines(&cfg)?;
    }
    if let Some(path) = seed_path {
        seed(&cfg, &path, dry)?;
    }
    loop {
        let open_prs = prs(&cfg)?;
        for mut pr in open_prs.clone() {
            if let Err(e) = tick(&cfg, &mut pr, dry) {
                log(
                    &cfg,
                    "tick_error",
                    json!({"pr":pr.number,"detail":e.to_string()}),
                );
            }
        }
        let active = prs(&cfg)?.into_iter().filter(|p| !p.terminal()).count();
        if once || !watch {
            log(
                &cfg,
                "pass_complete",
                json!({"open_prs":open_prs.len(),"still_working":active}),
            );
            return Ok(());
        }
        if active == 0 {
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
fn gh_json(cfg: &Config, args: &[&str]) -> Value {
    gh(cfg, args, false)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
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
fn latest_run(cfg: &Config, branch: &str, sha: Option<&str>) -> Option<Value> {
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
    )
    .as_array()?
    .clone();
    rows.into_iter().find(|r| {
        r["status"] == "completed"
            && sha.is_none_or(|wanted| {
                r["headSha"]
                    .as_str()
                    .is_some_and(|s| s.starts_with(&wanted[..wanted.len().min(12)]))
            })
    })
}
fn failure_set(cfg: &Config, id: u64) -> BTreeSet<String> {
    let id = id.to_string();
    let mut set = parse_failure_set(
        &gh(cfg, &["run", "view", &id, "--log-failed"], false).unwrap_or_default(),
    );
    let jobs = gh_json(cfg, &["run", "view", &id, "--json", "jobs"]);
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
    set.into_iter()
        .filter(|key| check_selected(cfg, key))
        .collect()
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
    let exact = latest_run(cfg, &cfg.base, Some(sha));
    let (run, provenance) = match exact {
        Some(r) => (Some(r), format!("exact:{short}")),
        None => {
            let r = latest_run(cfg, &cfg.base, None);
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
    Ok(gh_json(
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
    )
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
fn ci_status(cfg: &Config, pr: &Pr) -> &'static str {
    let number = pr.number.to_string();
    let value = gh_json(cfg, &["pr", "checks", &number, "--json", "state,name"]);
    let Some(rows) = value.as_array() else {
        return "none";
    };
    check_status(&cfg.check_names, rows)
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
    v.get("session").unwrap_or(&v)["status"]
        .as_str()
        .map(|s| s.to_ascii_lowercase())
}
fn pending(cfg: &Config, number: u64) -> PathBuf {
    cfg.state()
        .join("pending_reviews")
        .join(format!("{number}.json"))
}
fn settle(cfg: &Config, pr: &mut Pr, dry: bool) -> Result<bool, Box<dyn std::error::Error>> {
    let path = pending(cfg, pr.number);
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(false);
    };
    let value: Value = serde_json::from_str(&text)?;
    let (Some(id), Some(round)) = (value["session_id"].as_str(), value["round"].as_u64()) else {
        if !dry {
            let _ = fs::remove_file(path);
        }
        return Ok(false);
    };
    match goal_status(cfg, id).as_deref() {
        None | Some("running" | "pending" | "starting" | "active" | "parked") => Ok(true),
        Some("succeeded") => {
            if !dry {
                label(cfg, pr, format!("shepherd:review-{round}"));
                let _ = fs::remove_file(path);
            }
            Ok(false)
        }
        Some(_) => {
            if !dry {
                let _ = fs::remove_file(path);
            }
            Ok(false)
        }
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
fn tick(cfg: &Config, pr: &mut Pr, dry: bool) -> Result<(), Box<dyn std::error::Error>> {
    if pr.terminal() {
        return Ok(());
    }
    match ci_status(cfg, pr) {
        "pending" | "none" => return Ok(()),
        _ => {}
    }
    if settle(cfg, pr, dry)? {
        return Ok(());
    }
    let run = latest_run(cfg, &pr.branch, None);
    let current = run
        .as_ref()
        .and_then(|r| r["databaseId"].as_u64())
        .map(|id| failure_set(cfg, id))
        .unwrap_or_default();
    let (base, provenance) = if pr.base_sha.is_empty() {
        (BTreeSet::new(), "no-base".into())
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
    let kicks = pr.count("shepherd:kickback-");
    if !new.is_empty() {
        if !pr.has(RERUN) {
            if !dry && let Some(id) = run.as_ref().and_then(|r| r["databaseId"].as_u64()) {
                let id = id.to_string();
                let _ = gh(cfg, &["run", "rerun", &id, "--failed"], false);
                label(cfg, pr, RERUN.into())
            }
            return Ok(());
        }
        if kicks >= cfg.max_kickbacks {
            if !dry {
                label(cfg, pr, BLOCKED.into())
            }
            return Ok(());
        }
        if active_goals(cfg) >= cfg.max_concurrent {
            return Ok(());
        }
        if !dry {
            let list = new
                .iter()
                .map(|s| format!("  - {s}"))
                .collect::<Vec<_>>()
                .join("\n");
            let prompt = format!(
                "Pull request #{} (branch `{}`: {}) introduced {} new CI failure(s).\n\nNew failures:\n{}\n\n{}Reproduce, fix, test, commit, and push to `{}`. Stay in scope.",
                pr.number,
                pr.branch,
                pr.title,
                new.len(),
                list,
                note(&old),
                pr.branch
            );
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
        return Ok(());
    }
    let reviews = pr.count("shepherd:review-");
    if reviews >= cfg.cold_reviews {
        if !dry {
            label(cfg, pr, READY.into())
        }
        return Ok(());
    }
    if active_goals(cfg) >= cfg.max_concurrent {
        return Ok(());
    }
    if !dry {
        let round = reviews + 1;
        let prompt = format!(
            "Cold review pull request #{} (branch `{}`: {}). Round {} of {}. Fetch origin, inspect `git diff origin/{}...HEAD`, find only real bugs, add a test for each real fix, then commit and push. If no real issue exists, push nothing.\n\n{}",
            pr.number,
            pr.branch,
            pr.title,
            round,
            cfg.cold_reviews,
            cfg.base,
            note(&old)
        );
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
        "test (ubuntu)\tTests\tX test a::b ... FAILED\ntest (windows)\tTests\tX test a::b ... FAILED\nclippy\tLint\tX error: could not compile `x`",
    );
    assert_eq!(got.len(), 3);
    assert!(got.contains("clippy|step:Lint"));
    let base: BTreeSet<String> = BTreeSet::from(["j|a".into(), "j|b".into()]);
    let head: BTreeSet<String> = BTreeSet::from(["j|a".into(), "j|c".into()]);
    assert_eq!(base.len(), head.len());
    assert!(head.difference(&base).next().is_some());
    println!("shepherd self-test: ok");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
