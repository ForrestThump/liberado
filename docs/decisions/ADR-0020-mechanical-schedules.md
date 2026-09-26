---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0020
open_items: false
---

# ADR-0020: Mechanical Schedules

| Field | Value |
|-------|-------|
| Status | accepted |
| Date | 2026-09-25 |
| ID | ADR-0020 |

## Context

Scheduled work was a prose goal. Every firing paid for a model call, and the result was appended to the operator's chat. Morning and evening briefs were not read. A nightly vault backup has to run when the model is down, and it has to move with the daemon when Liberado is deployed on another machine.

## Decision

A schedule may set `job` to a built-in kind. The daemon runs that kind directly. It does not classify the goal and it does not start a session.

- `git-snapshot` commits the vault when it is dirty and pushes. It also runs once at startup unless `run_on_start = false`.
- `task-ping` reads open `- [ ]` lines from the vault and sends a ranked list.
- `habit-ping` sends `habit_text` unchanged.
- `inbox-if-present` dispatches `goal` only when the capture file has text. An empty file does not call a model.

Reminders go out through `LIBERADO_REMINDER_BOT_TOKEN` and `LIBERADO_REMINDER_CHAT_ID`. They do not use the sticky chat. If those variables are unset, the job still runs and the text is logged.

A host crontab is not the product schedule. The daemon already owns the clock. The job body is `git` and the filesystem, not a second scheduler and not an agent.

## Consequences

Vault edits are reversible when `git-snapshot` can push. Permission zones stay a denylist for folders a snapshot cannot make safe (`00 - Meta`, `Legal`, `proposals`, `.git`). The chat cron delivery path is unchanged for schedules that have no `job`.

The process has to be running. A crash loses the fine-grained nightly commits until the next start, which commits whatever is dirty.

## Rejected alternatives

A host crontab as the only backup. It does not travel with a Liberado deploy.

An agent goal that decides whether to commit. That depends on the model the backup exists to outlive.

A config key that runs an arbitrary shell command. That is a remote-exec hatch on a daemon with a writable vault.

## Implementation and tests

- `crates/cron` carries the job on the event.
- `crates/daemon/src/jobs.rs` executes it.
- `liberado_notify::TelegramNotifier::from_reminder_env` is the reminder channel.

## Supersedes / superseded by

- **Supersedes:** (none)
- **Superseded by:** (none)
