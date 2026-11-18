# turbovault-plugin-tasks

Obsidian-tasks vertical for TurboVault, shipped as a **compiled-in, default-off
plugin module**. Enable it with the host `tasks` feature:

```bash
cargo run -p turbovault --features tasks
```

Tools are advertised namespaced under the plugin id `tasks`:

| Tool | Kind | Purpose |
|---|---|---|
| `tasks_create`   | write | append a task line; the global-filter tag is added automatically |
| `tasks_list`     | read  | list tasks (status + tag filters). Each task includes `status`: `todo`, `in_progress`, `done`, or `cancelled` |
| `tasks_overdue`  | read  | open tasks (todo or in progress) past their due date |
| `tasks_tags`     | read  | distinct inline task tags |
| `tasks_complete` | write | flip checkbox to `[x]` + stamp `✅ <date>`; spawn the next occurrence of a recurring task |
| `tasks_update`   | write | edit fields (incl. `add_tags`/`remove_tags`) and re-render the line in the vault's dialect |
| `tasks_delete`   | write | remove a task line |
| `tasks_config`   | read  | show the settings the module tuned itself to |

These extract the full historical task tool surface (`list_tasks`,
`get_overdue_tasks`, `list_task_tags`, `complete_task`, `update_task`,
`delete_task`) onto the plugin boundary, plus `tasks_config`.

All vault access goes through the curated `VaultApi`; every write is
compare-and-swap (`Match(version)`) — the module cannot blind-overwrite a note.

## Round trip

Reads parse task lines with core's `turbovault_parser::parse_tasks`. Writes that
change fields (`tasks_update`) re-render through this crate's own renderer
([`render::to_markdown_line`] / `render_body`) — the write half of the round
trip, owned here because emitting a task requires choosing one canonical dialect
(an opinion), whereas parsing is tolerant and belongs to the kernel. The
invariant the renderer guarantees is semantic: `parse_tasks(render(task)) ==
task` for every field the parser models.

`tasks_complete`/`tasks_delete` still use lossless line surgery — they don't need
to re-render.

## Self-tuning to your Obsidian Tasks settings

The module matches your vault instead of requiring separate configuration. It
resolves two settings, in priority order:

1. **Authoritative** — `.obsidian/plugins/obsidian-tasks-plugin/data.json`, read
   through the curated `VaultApi::read_config` (a read-only capability scoped to
   `.obsidian/`). It picks up `taskFormat` (emoji vs dataview) and `globalFilter`.
2. **Heuristic** — when the settings file is absent or the host declines config
   reads, it infers the metadata dialect from the vault's existing task lines.
3. **Default** — emoji, no global filter.

`tasks_config` reports the resolved settings and their `source`
(`obsidian-data` | `heuristic` | `default`). The global filter, when set, scopes
every read to the tasks your Tasks plugin would consider real.

`tasks_list` status `pending`, `open`, `todo`, and `incomplete` return todo and
in-progress tasks. Cancelled (`[-]`) is not open. `completed` and `done` return
done tasks only. `cancelled` returns cancelled tasks only. `all` returns every
status.

Library callers can apply the same filter without the MCP tool:

```rust
let config = TasksConfig::from_obsidian_data(data_json).expect("tasks data.json");
let open: Vec<_> = turbovault_parser::parse_tasks(note)
    .into_iter()
    .filter(|task| config.accepts(task))
    .filter(|task| task.status.is_open())
    .collect();
```

`TasksConfig::accepts` matches a `#tag` filter as a case-insensitive tag token
(`#task` matches `#Task`, not `#tasks` or `#taskforce`). Any other filter is a
case-insensitive substring of the description. `TasksConfig::display_text`
removes that filter from the description when `removeGlobalFilter` is set.

> **Note:** `VaultApi::read_config` is a fork-local addition to the plugin API
> (a non-breaking default method, `None` where a host declines it). It is the
> only door onto the vault's non-note config space and is flagged for upstream
> review before it is proposed to turbovault-core.

## Recurrence

Completing a task with a recurrence rule (`🔁 every week`, `every 2 days`,
`every weekday`, `every month`, …) spawns the next occurrence as a fresh open
task directly below the completed one, with its due/scheduled/start dates
advanced by the interval (see `recurrence.rs`). The completed line is edited
losslessly; the spawned line is rendered in the detected dialect.
