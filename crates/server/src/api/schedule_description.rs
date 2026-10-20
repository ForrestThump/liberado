//! Plain-English description of one `[[schedules]]` entry.
//!
//! The sentences follow the daemon's job kinds. A prompt schedule says which
//! profile runs, and quotes a short copy of the goal. An unknown kind does not
//! get a guessed action.

use liberado_config::CronSchedule;

const PROMPT_LIMIT: usize = 120;
const REMINDER: &str =
    " No model session. The reminder goes to the reminder channel, not the sticky chat.";

pub(crate) fn schedule_description(schedule: &CronSchedule) -> String {
    match nonempty(schedule.job.as_deref()) {
        None => prompt_description(schedule),
        Some(kind) => job_description(schedule, kind),
    }
}

/// `system` for the vault-reminder engine. Every other row is an automation.
///
/// The tick delivers Obsidian `#remind` pings. It is plumbing, not a reminder.
pub(crate) fn schedule_category(job: Option<&str>) -> &'static str {
    match job.map(str::trim) {
        Some("vault-reminder-tick") => "system",
        _ => "automation",
    }
}

fn job_description(schedule: &CronSchedule, kind: &str) -> String {
    match kind {
        "event-ping" => event_ping(),
        "task-ping" => task_ping(schedule),
        "habit-ping" => habit_ping(schedule),
        "vault-reminder-tick" => reminder_tick(),
        "git-snapshot" => git_snapshot(schedule),
        "inbox-if-present" => inbox_description(schedule),
        other => unknown_job(schedule, other),
    }
}

fn event_ping() -> String {
    format!(
        "Sends a reminder listing up to 10 calendar events from today through the next 7 days.{REMINDER}"
    )
}

fn task_ping(schedule: &CronSchedule) -> String {
    let limit = schedule.task_limit.filter(|n| *n > 0).unwrap_or(10);
    format!("Sends a reminder listing up to {limit} open vault tasks.{REMINDER}")
}

fn habit_ping(schedule: &CronSchedule) -> String {
    match nonempty(schedule.habit_text.as_deref()) {
        Some(text) => {
            let collapsed = collapse(text);
            format!(
                "Sends this reminder: {}{REMINDER}",
                stopped(&quote(&collapsed), &collapsed)
            )
        }
        None => format!("Sends a habit reminder, but no habit text is set.{REMINDER}"),
    }
}

fn reminder_tick() -> String {
    "Background system job. Each minute, delivers the timed pings for Obsidian tasks and calendar notes you marked #remind / remind: true. Stays quiet when nothing is due or reminders are off. Your reminders themselves live in the vault, not here.".to_string()
}

fn git_snapshot(schedule: &CronSchedule) -> String {
    let remote = nonempty(schedule.git_remote.as_deref()).unwrap_or("origin");
    let push = match nonempty(schedule.git_branch.as_deref()) {
        Some(branch) => format!("pushes HEAD to branch \"{branch}\" on \"{remote}\""),
        None => format!("pushes HEAD to \"{remote}\""),
    };
    let startup = if schedule.run_on_start == Some(false) {
        ""
    } else {
        " Also runs once at daemon start when the tree is dirty."
    };
    format!(
        "When the vault git tree is dirty, commits a snapshot and {push}. Stays quiet when the tree is clean. Sends a reminder if the snapshot fails. No model session.{startup}"
    )
}

fn inbox_description(schedule: &CronSchedule) -> String {
    let path = nonempty(schedule.capture_path.as_deref()).unwrap_or("Inbox/Capture.md");
    format!(
        "When \"{path}\" has capture text, {} {} {} When that file is empty, does nothing.",
        run_lead(schedule),
        stopped(&prompt_clause(&schedule.goal), &schedule.goal),
        delivery_sentence(schedule.deliver)
    )
}

fn prompt_description(schedule: &CronSchedule) -> String {
    format!(
        "{} {} {}",
        capitalize(&run_lead(schedule)),
        stopped(&prompt_clause(&schedule.goal), &schedule.goal),
        delivery_sentence(schedule.deliver)
    )
}

fn unknown_job(schedule: &CronSchedule, kind: &str) -> String {
    let mut text = format!("Configured job \"{kind}\".");
    if let Some(profile) = nonempty(schedule.profile.as_deref()) {
        text.push_str(&format!(" Profile \"{profile}\"."));
    }
    if let Some(habit) = nonempty(schedule.habit_text.as_deref()) {
        let collapsed = collapse(habit);
        text.push_str(&format!(
            " Habit text {}",
            stopped(&quote(&collapsed), &collapsed)
        ));
    }
    let goal = collapse(&schedule.goal);
    if !goal.is_empty() {
        text.push_str(&format!(" Prompt: {}", stopped(&quote(&goal), &goal)));
    }
    text.push_str(" No behavior is defined for this kind.");
    text
}

fn run_lead(schedule: &CronSchedule) -> String {
    let who = match nonempty(schedule.profile.as_deref()) {
        Some(profile) => format!("the \"{profile}\" agent"),
        None => "an agent".to_string(),
    };
    if schedule.direct == Some(true) {
        format!("skips the router and runs {who}")
    } else {
        format!("runs {who}")
    }
}

fn prompt_clause(goal: &str) -> String {
    let collapsed = collapse(goal);
    if collapsed.is_empty() {
        "with an empty prompt".to_string()
    } else {
        format!("with the prompt: {}", quote(&collapsed))
    }
}

/// Close `clause` with one period. A goal that is fully shown and already ends
/// in `.`, `!`, or `?` keeps that mark. The description does not add a second one.
fn stopped(clause: &str, source: &str) -> String {
    if source_keeps_its_stop(source) {
        clause.to_string()
    } else {
        format!("{clause}.")
    }
}

fn source_keeps_its_stop(source: &str) -> bool {
    let collapsed = collapse(source);
    collapsed.chars().nth(PROMPT_LIMIT).is_none()
        && matches!(collapsed.chars().next_back(), Some('.' | '!' | '?'))
}

fn delivery_sentence(deliver: Option<bool>) -> &'static str {
    if deliver == Some(false) {
        "The result is not pushed to the notifier."
    } else {
        "The result is pushed to the notifier when the run finishes."
    }
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn quote(text: &str) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(PROMPT_LIMIT).collect();
    if chars.next().is_some() {
        format!("\"{head}…\"")
    } else {
        format!("\"{head}\"")
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

#[cfg(test)]
#[path = "schedule_description_tests.rs"]
mod tests;
