---
kind: finding
status: active
authority: advisory
domain: coding-harness
canonical_for: tool-result-hint-hooks
open_items: true
---

# Tool-result hint hooks (idea)

**Status**: Idea only, 2026-08-13. No code. Do not schedule this ahead of
measured finish-loop work. Recorded so the shape is not lost.

**From**: compare 4 (B1) discussion. Same Flash, pi stayed in one session;
Liberado retried after the 50-turn cap and the ship bar. A “remind after
`git commit`” hook would not have fired — the coding pack tells the model
not to commit, and the B1 run used `status` / `diff` / `stash` only.

---

## The idea

A small TOML table: if a `run_command` (or bash) call matches, append a
note to that tool’s result JSON.

Not a new tool. Not a side channel. The model will treat the note as the
next task — same class of risk as a compile bit after every edit.

Match on `program` + argv (`program == git` and `args[0] == commit`), not
a substring in a blob. `git commit` appears in help text and log lines.

## Do not start with `git commit` → “run CI”

Liberado refuses `submit_report outcome=succeeded` while `cargo check` is
red (same conversation, PR #163), then still runs `cargo test --workspace`
after a green compile. A commit reminder is late, and on this path it often
never happens.

The B1 misses were earlier:

| Match | Note that would have helped |
|---|---|
| `cargo test -p` | The bar is `cargo test --workspace` |
| several successful `edit_file`s and no `cargo check` yet | Compile the crate you touched |
| `submit_report` with outcome succeeded | Do not claim a green `-p` suite |

`git commit` → “consider CI” fits a bash CLI with no ship bar (pi). It is
the wrong first rule here.

## If this is built later

Keep it a hook table, not a one-off `if commit`. Start with the `-p` /
“no check since last edit” rules. Those match the traces.
