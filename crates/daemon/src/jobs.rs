//! Mechanical schedules: git snapshot, task list, habit ping, inbox gate.
//!
//! These run in the daemon. They do not call a model. A reminder, when there is one, goes
//! to the reminder notifier — never the sticky chat.

use std::fs;
use std::path::{Path, PathBuf};

use liberado_common::Event;
use liberado_common::process::std_command;

use crate::types::{Daemon, ReactionOutcome};

const DEFAULT_TASK_LIMIT: u32 = 10;
const DEFAULT_CAPTURE: &str = "Inbox/Capture.md";
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".obsidian",
    ".trash",
    "00 - Meta",
    "Legal",
    "proposals",
];

/// Optional fields a schedule or event may set. Empty values fall back to defaults.
#[derive(Debug, Clone, Default)]
pub struct JobOptions {
    pub habit_text: Option<String>,
    pub task_limit: Option<u32>,
    pub capture_path: Option<String>,
    pub git_remote: Option<String>,
    pub git_branch: Option<String>,
    pub git_user_name: Option<String>,
    pub git_user_email: Option<String>,
}

/// One mechanical firing, with defaults already filled.
#[derive(Debug, Clone)]
pub struct JobRequest {
    pub name: String,
    pub kind: String,
    pub habit_text: Option<String>,
    pub task_limit: u32,
    pub capture_path: String,
    pub git_remote: String,
    pub git_branch: Option<String>,
    pub git_user_name: String,
    pub git_user_email: String,
}

impl JobRequest {
    /// Build from the cron event payload. `None` when the event is not a mechanical job.
    pub fn from_event(name: &str, data: &serde_json::Value) -> Option<Self> {
        let kind = data.get("job").and_then(|v| v.as_str())?;
        let text = |key: &str| data.get(key).and_then(|v| v.as_str()).map(str::to_string);
        Some(Self::from_options(
            name,
            kind,
            JobOptions {
                habit_text: text("habit_text"),
                task_limit: data
                    .get("task_limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32),
                capture_path: text("capture_path"),
                git_remote: text("git_remote"),
                git_branch: text("git_branch"),
                git_user_name: text("git_user_name"),
                git_user_email: text("git_user_email"),
            },
        ))
    }

    pub fn from_options(name: &str, kind: &str, options: JobOptions) -> Self {
        Self {
            name: name.to_string(),
            kind: kind.to_string(),
            habit_text: options.habit_text,
            task_limit: options
                .task_limit
                .filter(|n| *n > 0)
                .unwrap_or(DEFAULT_TASK_LIMIT),
            capture_path: options
                .capture_path
                .filter(|path| !path.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_CAPTURE.to_string()),
            git_remote: options
                .git_remote
                .filter(|remote| !remote.trim().is_empty())
                .unwrap_or_else(|| "origin".to_string()),
            git_branch: options
                .git_branch
                .filter(|branch| !branch.trim().is_empty()),
            git_user_name: options
                .git_user_name
                .filter(|user| !user.trim().is_empty())
                .unwrap_or_else(|| "Liberado".to_string()),
            git_user_email: options
                .git_user_email
                .filter(|email| !email.trim().is_empty())
                .unwrap_or_else(|| "liberado@localhost".to_string()),
        }
    }
}

/// What the reactor should do with a mechanical result.
#[derive(Debug, PartialEq, Eq)]
pub enum JobEffect {
    /// Nothing to tell the human.
    Quiet,
    /// Send this text on the reminder channel.
    Remind(String),
    /// The inbox has captures. Dispatch the schedule goal.
    Continue,
}

pub fn execute(root: &Path, job: &JobRequest) -> JobEffect {
    match job.kind.as_str() {
        "git-snapshot" => git_snapshot(root, job),
        "task-ping" => JobEffect::Remind(task_message(root, job.task_limit)),
        "habit-ping" => match job
            .habit_text
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(text) => JobEffect::Remind(text.to_string()),
            None => JobEffect::Remind(format!("{} has no habit text", job.name)),
        },
        "inbox-if-present" => {
            if capture_is_empty(root, &job.capture_path) {
                JobEffect::Quiet
            } else {
                JobEffect::Continue
            }
        }
        other => JobEffect::Remind(format!("{} has unknown job '{other}'", job.name)),
    }
}

impl Daemon {
    /// `Some` when this event was a mechanical job and is finished.
    /// `None` when it is not mechanical, or the inbox gate found captures and the goal
    /// should dispatch.
    pub(crate) fn handle_mechanical(&self, event: &Event) -> Option<ReactionOutcome> {
        let name = crate::helpers::cron_schedule_name(&event.source).unwrap_or("job");
        let request = JobRequest::from_event(name, &event.payload.data)?;
        match execute(self.vault.root(), &request) {
            JobEffect::Continue => None,
            JobEffect::Quiet => {
                tracing::info!(job = %request.name, kind = %request.kind, "mechanical job quiet");
                Some(ReactionOutcome::Observed)
            }
            JobEffect::Remind(text) => {
                tracing::info!(job = %request.name, kind = %request.kind, "mechanical job reminder");
                self.send_reminder(&text);
                Some(ReactionOutcome::Observed)
            }
        }
    }

    pub(crate) fn run_startup_jobs(&self) {
        for job in &self.startup_jobs {
            match execute(self.vault.root(), job) {
                JobEffect::Quiet => {
                    tracing::info!(job = %job.name, "startup snapshot clean");
                }
                JobEffect::Remind(text) => {
                    tracing::warn!(job = %job.name, "startup snapshot needs attention");
                    self.send_reminder(&text);
                }
                JobEffect::Continue => {}
            }
        }
    }

    fn send_reminder(&self, text: &str) {
        let Some(notifier) = self.reminder.clone() else {
            tracing::warn!(%text, "reminder bot is not configured; message not sent");
            return;
        };
        let text = text.to_string();
        tokio::spawn(async move {
            if let Err(e) = notifier.notify(&text).await {
                tracing::warn!(error = %e, "reminder delivery failed");
            }
        });
    }
}

fn git_snapshot(root: &Path, job: &JobRequest) -> JobEffect {
    if !root.join(".git").is_dir() {
        return JobEffect::Remind(format!(
            "{}: {} is not a git repository",
            job.name,
            root.display()
        ));
    }
    let status = match git(root, job, &["status", "--porcelain"]) {
        Ok(output) => output,
        Err(error) => return JobEffect::Remind(format!("{}: {error}", job.name)),
    };
    if !status.status.success() {
        return JobEffect::Remind(format!(
            "{}: git status failed: {}",
            job.name,
            output_text(&status)
        ));
    }
    if status.stdout.iter().all(u8::is_ascii_whitespace) {
        return JobEffect::Quiet;
    }
    if let Err(error) = git_ok(root, job, &["add", "-A"]) {
        return JobEffect::Remind(format!("{}: {error}", job.name));
    }
    let message = format!("vault snapshot {}", chrono::Utc::now().format("%Y-%m-%d"));
    if let Err(error) = git_ok(root, job, &["commit", "-m", &message]) {
        return JobEffect::Remind(format!("{}: {error}", job.name));
    }
    let spec = job
        .git_branch
        .as_deref()
        .map(|branch| format!("HEAD:refs/heads/{branch}"))
        .unwrap_or_else(|| "HEAD".to_string());
    if let Err(error) = git_ok(root, job, &["push", &job.git_remote, &spec]) {
        return JobEffect::Remind(format!(
            "{}: committed locally; push failed: {error}",
            job.name
        ));
    }
    JobEffect::Quiet
}

fn git(root: &Path, job: &JobRequest, args: &[&str]) -> Result<std::process::Output, String> {
    let safe = format!("safe.directory={}", root.display());
    let name = format!("user.name={}", job.git_user_name);
    let email = format!("user.email={}", job.git_user_email);
    std_command("git")
        .current_dir(root)
        .arg("-c")
        .arg(&safe)
        .arg("-c")
        .arg(&name)
        .arg("-c")
        .arg(&email)
        .args(args)
        .output()
        .map_err(|error| format!("git failed to start: {error}"))
}

fn git_ok(root: &Path, job: &JobRequest, args: &[&str]) -> Result<(), String> {
    let output = git(root, job, args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("git {} failed: {}", args[0], output_text(&output)))
    }
}

fn output_text(output: &std::process::Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if text.is_empty() {
        text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    }
    let mut chars = text.chars();
    let short: String = chars.by_ref().take(300).collect();
    if chars.next().is_some() {
        format!("{short}…")
    } else {
        short
    }
}

#[derive(Debug)]
struct OpenTask {
    priority: u8,
    due: Option<String>,
    text: String,
    path: String,
}

fn task_message(root: &Path, limit: u32) -> String {
    let mut tasks = Vec::new();
    collect_tasks(root, root, &mut tasks);
    tasks.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| match (&a.due, &b.due) {
                (Some(left), Some(right)) => left.cmp(right),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.text.cmp(&b.text),
            })
    });
    if tasks.is_empty() {
        return "No open tasks.".to_string();
    }
    let mut lines = vec!["Open tasks".to_string(), String::new()];
    for (index, task) in tasks.into_iter().take(limit as usize).enumerate() {
        let due = task
            .due
            .map(|date| format!(" — due {date}"))
            .unwrap_or_default();
        lines.push(format!("{}. {}{due} — {}", index + 1, task.text, task.path));
    }
    lines.push(String::new());
    lines.push("Reply in Liberado when you want one done.".to_string());
    lines.join("\n")
}

fn collect_tasks(root: &Path, dir: &Path, out: &mut Vec<OpenTask>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if SKIP_DIRS.contains(&name.as_ref()) || name.starts_with('.') {
                continue;
            }
            collect_tasks(root, &path, out);
            continue;
        }
        if name.ends_with(".md") {
            read_tasks(root, &path, out);
        }
    }
}

fn read_tasks(root: &Path, path: &Path, out: &mut Vec<OpenTask>) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    for line in text.lines() {
        let trimmed = line.trim();
        let Some(body) = trimmed.strip_prefix("- [ ]") else {
            continue;
        };
        let body = body.trim();
        if body.is_empty() {
            continue;
        }
        out.push(OpenTask {
            priority: task_priority(body),
            due: task_due(body),
            text: body.to_string(),
            path: rel.clone(),
        });
    }
}

fn task_priority(line: &str) -> u8 {
    if line.contains('⏫') {
        3
    } else if line.contains('🔼') {
        2
    } else if line.contains('🔽') {
        0
    } else {
        1
    }
}

fn task_due(line: &str) -> Option<String> {
    let rest = line.split('📅').nth(1)?.trim_start();
    let date: String = rest.chars().take(10).collect();
    if date.len() == 10
        && date.as_bytes().get(4) == Some(&b'-')
        && date.as_bytes().get(7) == Some(&b'-')
        && date.bytes().all(|b| b.is_ascii_digit() || b == b'-')
    {
        Some(date)
    } else {
        None
    }
}

pub fn capture_is_empty(root: &Path, rel: &str) -> bool {
    let Some(path) = safe_join(root, rel) else {
        return true;
    };
    let Ok(text) = fs::read_to_string(path) else {
        return true;
    };
    let body = match text.split_once("## Capture") {
        Some((_, after)) => after
            .split_once("\n## ")
            .map(|(section, _)| section)
            .unwrap_or(after),
        None => text.as_str(),
    };
    body.trim().is_empty()
}

#[cfg(test)]
#[path = "jobs_tests.rs"]
mod jobs_tests;

fn safe_join(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel = rel.trim().trim_start_matches('/');
    if rel.is_empty() || rel.contains("..") {
        return None;
    }
    Some(root.join(rel))
}
