> ⚠️ Archive — see living copy at `docs/project/handoff.md`. This file is preserved for historical reference only.

# Liberado â€” Handoff (2026-07-08)

Current-state handoff. Kept in sync after each session arc. For the authoritative system map read
[`../../../spec/architecture/overview.md`](../../../spec/architecture/overview.md); for build/test read
the root [`AGENTS.md`](../../../../AGENTS.md); for run/configure read
[`crates/cli/ARCHITECTURE.md`](../../../../crates/cli/ARCHITECTURE.md) and
[`docs/spec/config-spec.md`](../../../spec/config-spec.md); for the development process (how to
research, plan, delegate, test, commit) read
[`../../../impl/development-workflow.md`](../../../impl/development-workflow.md); for the chat
API contract read [`../../../spec/reference/api.md`](../../../spec/reference/api.md); for the rationale behind any
"Decision N" read [`../../../spec/architecture-decisions.md`](../../../spec/architecture-decisions.md);
for open human-only action items read [`../../../future-work/archive/human-todo.md`](../../../future-work/archive/human-todo.md) â€” it's
more current than this file on anything it covers, since it's updated the moment an item lands rather
than at session-arc boundaries.

> Note: keep this file lowercase (`handoff.md`, not `HANDOFF.md`). On Windows (case-insensitive
> filesystem) the two names collide â€” see Pitfalls below.

---

## What's built (as of 2026-07-08)

All nineteen "Done" milestones in [`../../../spec/architecture/overview.md`](../../../spec/architecture/overview.md) are
shipped and `cargo test --workspace` is green (unchanged this session â€” no life-os source was
touched; all work below happened in sibling/standalone repos and this session's own tooling setup).
The system is:

- **One `liberado` binary** (daemon-first, Decision 2): `liberado serve [vault]` runs the daemon + chat
  + HTTP/SSE API; `liberado chat [session]` is a native client; bare `liberado <vault>` aliases `serve`.
- **Phases 1-3 complete, Phase 4 v1 landed 2026-07-07**: chat routes through the dispatcher; capability
  catalog is live and shared; `crates/tui` is a ratatui client; web UI has sidebar/MCP panel/Markdown/
  slash commands; self-improvement (`liberado-pr-dispatch-mcp`, nÃ©e `riggers/`) enables draft-PR-only
  self-extension; cron + external webhooks + named dispatcher/executor pools are all live; Docker MCP
  transport (`McpTransport::Docker`) is built and unit-tested, with its live Docker-daemon smoke test
  still the one open item (see `human-todo.md`).
- **Strategic pivot, 2026-07-09**: the draft-PR self-improvement workflow stays, but `vtcode` is no
  longer the long-term coding engine. The direction is a Rust-native **agentic orchestration** kernel
  (goal loops, verifiers, critics, subagents, session/events for TUI/WebUI) on `Provider` +
  `Executor` + `ToolRuntime`, with coding as the first domain. Crates:
  `coder-core`/`coder-tools`/`coder-agent`/`coder-sandbox`/`coder-runner`. Read
  [`../../../spec/architecture/agentic-loops.md`](../../../spec/architecture/agentic-loops.md) and
  [`../../../future-work/rust-native-agentic-coder-plan.md`](../../../future-work/rust-native-agentic-coder-plan.md)
  before doing new PR-dispatch or coding-agent work.
- **`liberado-pr-dispatch-mcp` proven end-to-end on Windows this session, with a real upstream bug
  found and fixed along the way** â€” see "This session's work" below. This validates Phase 2's
  self-improvement engine on a second platform; it previously only ran on the Linux/Docker homelab
  deployment.
- **`liberado-deliberate-mcp`** (multi-model deliberation MCP, standalone repo, fully decoupled from
  life-os as of 2026-07-06) is built, configured against the real vault, and registered as an MCP
  server for this Claude Code project â€” see "This session's work" below.

**Not yet built (next slice)**: the Rust-native coding backend described above, multi-MCP registry UX,
connection pooling, and the remaining `liberado-common` decomposition. External webhook hooks are
already built; an inbox-specific hook/workflow may still be future work depending on product direction.
See [`../../../roadmap.md`](../../../roadmap.md) for the full list.

---

## This session's work (2026-07-08)

Two threads, both outside `life-os`'s own tracked source (no `crates/*` or workspace file was
touched):

**1. Proved the PR-dispatch pipeline on Windows before "shotgunning" a batch of WebUI tasks through
it**, per your request to test on one low-risk real task first. It failed twice before succeeding,
and every failure was a real bug:

- Six fixes landed in `liberado-pr-dispatch-mcp` (env-var path overrides instead of hardcoded Unix
  paths, a Windows `git-askpass` helper, a Git-Credential-Manager bypass for the specific git
  invocations, stdin-piped prompts instead of a CLI arg to dodge Windows' command-line length limit,
  a TOML literal string to survive Windows path backslashes, and excluding vtcode's own `.vtcode/`
  scratch directory from commits). **Committed 2026-07-08** (`5157b21`, local only, not pushed).
- Found and fixed a genuine bug in upstream `vtcode` itself (not this project, not a config mistake â€”
  confirmed by reading the actual source across three versions): primary-agent model resolution
  clobbered a real configured model with the literal string `"inherit"`. Fixed on
  [ForrestThump/VTCode](https://github.com/ForrestThump/VTCode) (branch
  `fix/primary-agent-inherit-model`); you opened
  [vinhnx/VTCode#697](https://github.com/vinhnx/VTCode/pull/697) upstream, and it **merged into
  `main` 2026-07-08** (`fe45c4e`) â€” no tagged release includes it yet, so
  `liberado-pr-dispatch-mcp` was cleaned up to build `vtcode` from git source tracking upstream
  `vinhnx/VTCode` `main` (not the fork) rather than depend on the fork or wait on a crates.io
  release; subagents (disabled as a workaround for this exact bug) are re-enabled. The
  `ForrestThump/VTCode` fork is no longer needed â€” safe to archive/delete whenever.
- The smoke test itself succeeded cleanly on the second attempt (patched fork binary):
  [ForrestThump/liberado#2](https://github.com/ForrestThump/liberado/pull/2), a single-line diff
  (a `title="Send (Enter)"` tooltip on the chat send button) â€” exactly the intended change. It's a
  **draft, awaiting your review/approval**; I did not approve or merge it.

Full detail, including the two throwaway/junk PRs closed along the way: `human-todo.md`'s
"`liberado-pr-dispatch-mcp` Windows portability pass" section.

**2. Kicked off a multi-model deliberation on WebUI polish/feature ideas**, per your request to
brainstorm a batch of ~10 small PR-dispatch tasks for a Saturday review session. In progress, paused
partway to do this doc refresh instead:

- Surveyed the current WebUI state (`crates/webui/src/components/`) â€” see "Where key things live"
  below for the read-out.
- Stood up the infrastructure: started a `turbovault` HTTP instance against your real vault
  (`C:/Users/Shiloh/Obsidian/Main`, port 3737 â€” found an earlier attempt's process had actually
  survived despite looking dead, so no fresh start was needed); registered both
  `liberado-deliberate-mcp` (stdio) and `liberado-pr-dispatch-mcp` (HTTP, against the same scratch
  dispatch server PR #2 came from) as MCP servers for this Claude Code project
  (`claude mcp add ... --scope local`), confirmed `Connected` via `claude mcp list`.
- **Blocked on a session restart**: Claude Code only loads a newly-registered MCP server's tools at
  session startup. Both servers are configured and healthy, but *this* running session can't call
  their tools yet. That's what triggered "start fresh and update the docs first."
- **Not yet done**: writing the actual context brief for the deliberation participants, running the
  deliberation rounds, picking the top 10 with you, and submitting them as `liberado-pr-dispatch-mcp`
  queue entries. All of that resumes in the fresh session, once its MCP tools are live.

---

## Where key things live

| What | Where |
|---|---|
| Shared type vocabulary | `crates/common/src/` |
| Proposal type + signer | `crates/common/src/proposal.rs` |
| Per-installation proposal signing key | `crates/config/src/lib.rs` (`load_or_create_proposal_key`), stored at `<data_dir>/.proposal-key` |
| Dispatcher (classify + guards) | `crates/dispatcher/src/` |
| Executor (agent loop) | `crates/executor/src/` â€” `RiskGatedToolRuntime` lives here |
| Orchestrator (bridges decision â†’ execution) | `crates/orchestrator/src/lib.rs` |
| Multi-turn conversation (chat loop) | `crates/main-agent/src/sessions.rs` (`ChatSessions`) |
| Daemon watch loop | `crates/daemon/src/lib.rs` â€” `handle_proposal_change` routes approved proposals |
| Shared env wiring | `crates/bootstrap/src/lib.rs` |
| Config loading + `GuardContext` | `crates/config/src/lib.rs` |
| Binary entry + arg dispatch | `crates/cli/src/main.rs` |
| Chat client (CLI) | `crates/cli/src/chat_client.rs` |
| Server library | `crates/server/src/lib.rs` â€” `run()` is the daemon entry point |
| TUI client | `crates/tui/src/` |
| Web UI (Dioxus WASM) | `crates/webui/src/` â€” `dx build` only, excluded from native workspace build |
| Conversation store | `crates/conversation-store/src/{jsonl,store,types,error}.rs` |
| Shared SSE decoder + slash-command dispatcher | `crates/chat-client-contract/` and `crates/liberado-commands/` |
| PR-dispatch self-improvement engine (standalone repo, own git remote) | `liberado-pr-dispatch-mcp/` â€” [ForrestThump/liberado-pr-dispatch-mcp](https://github.com/ForrestThump/liberado-pr-dispatch-mcp) |
| Multi-model deliberation engine (standalone repo) | `liberado-deliberate-mcp/` â€” config at `%APPDATA%\liberado-deliberate-mcp\deliberation.toml` |
| Shared no-life-os-dependency crates (`turbovault-client`, `frontmatter-note`, `openai-compat-client`) | [liberado-standalone-kit](https://github.com/ForrestThump/liberado-standalone-kit) (private), pinned `git` dep of both MCPs above |

### WebUI component read-out (for the in-flight deliberation)

`crates/webui/src/main.rs` renders a header (Chat/Status nav) over a `Sidebar` + main content area
(`Chat` or `Dashboard`). Components, each in `crates/webui/src/components/`:

- **`chat.rs`** â€” the chat pane: message bubbles (user/assistant/tool/system/error), collapsible
  "thinking steps" (tool call + result, pending/ok/err), an auto-growing textarea, SSE streaming
  (`token`/`tool`/`tool_result`/`done`/`failed` events), stop-generating, and slash-command dispatch.
  Auto-titles a new conversation from its first message after the stream completes.
- **`sidebar.rs`** â€” conversation list (relative-time labels), live search-as-you-type, "+ New Chat",
  collapse/expand (auto-collapsed under 768px), hosts `McpPanel` in its footer.
- **`dashboard.rs`** â€” daemon status banner (running/uptime/vault path/watcher/dispatcher/model/
  reactions-seen) plus a two-panel grid: `VaultPanel` (root/note-count/watcher) and `ReactionsPanel`
  (last 20 reactions: event type, outcome badge, path, time).
- **`mcp_panel.rs`** â€” collapsible registered-MCP list with tool counts, consequence badges
  (read_only/reversible/irreversible/external), and per-server expand showing tool names, main-agent/
  dispatcher visibility badges, and provenance.
- **`slash_commands.rs`** â€” `/new`, `/clear`, `/session ...`, `/status`, `/model`, `/help`, etc., via
  `liberado-commands`' shared `CommandContext` trait; results not needing a network round trip are
  instant. No modal/picker widget yet â€” `/theme list`/`/session list` fall back to a numbered
  plain-text list.
- **`markdown.rs`**, **`reactions.rs`**, **`vault.rs`** â€” small, focused renderers/fetchers backing
  the above.

No dark/light theme toggle in the WebUI yet (`set_theme` is a hardcoded no-op returning `false`);
no in-chat image/file upload; no conversation delete/rename/pin from the sidebar; no keyboard-shortcut
help; no unread/typing indicators; no per-message copy/regenerate/edit actions. These are exactly the
shape of gap the in-flight deliberation is aimed at.

---

## Working patterns that succeed (keep doing)

- **Research before design â€” verify claims against code.** Read the struct/function/route before
  trusting a doc that describes it.
- **Dispatch parallel research subagents for audit-shaped work**, one per independent angle, each with
  full context, a narrow angle, a demand for file:line verdicts, and a word budget.
- **Verify code-staleness claims against code, not narrative.**
- **Cheap link-checking catches bugs human review misses** â€” regex-extract markdown links, verify
  each target path.
- **Dispatch a subagent for code, then verify independently**: read the produced code, `cargo build`
  then `cargo test --workspace --no-run` (different from build â€” compiles `#[cfg(test)]`), then
  `cargo test --workspace`.
- **Live smoke recipe** (proven repeatedly): hydrate secrets from env (confirm only length/prefix,
  never print); start on a scratch port/data-dir; drive a real request through; assert the actual
  external side effect (a file written, a PR opened), not just an exit code.
- **Rigorously confirm an external-dependency bug before treating it as real**: read the actual source
  across multiple versions (crates.io-published *and* a fresh git clone of HEAD), compare the buggy
  path against an analogous *correct* path in the same codebase, check `git log --grep` for whether
  it was ever addressed. This is what turned "plausible vtcode bug" into "certain, with a byte-level
  fix" this session.
- **When a fix works, write the PR case for someone else's project like you're the one being
  reviewed**: root cause, side-by-side code comparison, how it was tested (including the live
  reproduction), a clean single commit â€” not a raw diff dump. Delivered as a copy-paste-able
  `*-pr-description.md` when the user is opening the PR themselves, not you.
- **Split audit into two commits**: first the findings doc (`docs/future-work/`), then the fix.

---

## Pitfalls learned (don't relearn)

- **Windows is case-insensitive**: `handoff.md` and `HANDOFF.md` collide. Keep one lowercase handoff.
- **`cargo build` is not enough** â€” does not compile `#[cfg(test)]`. Always run
  `cargo test --workspace --no-run` separately, then `cargo test --workspace`.
- **`cargo install` ignores the crate's own `Cargo.lock` by default** â€” use `--locked`, or you can hit
  a compile failure (a dependency version mismatch) that the crate's own CI never sees.
- **Double-backgrounding a process is fragile on this setup**: `(cmd &) && sleep N && curl ...` inside
  one Bash-tool call, itself run with `run_in_background`, can let the inner backgrounded process die
  with the outer wrapper instead of surviving independently. Prefer handing the actual long-running
  command directly to the tool's own `run_in_background: true`, not a shell-level `&` inside it.
- **A "bind: address already in use" failure on a port you just tried to start something on** usually
  means an *earlier* attempt actually succeeded and is still alive (didn't die the way it looked like
  it did) â€” check `netstat -ano | grep :<port>` and just reuse it before assuming the new start
  failed.
- **Windows' Git Credential Manager intercepts auth before `GIT_ASKPASS` is consulted** and hangs
  non-interactively if no credential is cached. Fix: `-c credential.helper=` on the specific git
  invocation (clone/push), not a global config change.
- **`gh`/`git` over HTTPS with `credential.helper=` disabled has no non-interactive auth path at all**
  â€” if `gh auth status` shows `ssh` as the Git protocol, switch the remote to `git@github.com:...`
  rather than fighting HTTPS credentials.
- **Windows' ~32K CLI argument length limit** (vs. Linux's ~2MB) â€” a long prompt/argument passed as a
  literal CLI arg can silently truncate or fail (`error 206`) on Windows where it'd work fine on the
  actual Linux deployment target. Pipe over stdin instead where the receiving program supports it.
- **TOML double-quoted strings interpret backslash escapes** â€” a Windows path written into a
  double-quoted TOML string can break on its own backslashes. Use a literal (single-quoted) TOML
  string for any value that might contain a raw Windows path.
- **`dirs::home_dir()` ignores `HOME` on Windows** (uses `SHGetKnownFolderPath` instead) â€” a
  per-process `HOME` override that works on Linux/Mac silently no-ops on Windows for anything using
  that crate. Windows-only quirk, irrelevant to an actual Linux deployment target; don't chase it as
  if it were a real bug.
- **A newly `claude mcp add`-ed server doesn't get its tools into an already-running Claude Code
  session** â€” registration + connection health (`claude mcp list` showing `Connected`) happens
  immediately, but the session's own tool list only refreshes at startup. Needs a restart/reload,
  every time, no exception found this session.
- **`claude mcp add` (run from a shell where cwd shows as `C:/...`) stores servers under a
  case-sensitive project key in `~/.claude.json`** â€” a *fresh* Claude Code session invoked with a
  lowercase-drive-letter cwd (`c:/...`) is a *different* key in `projects{}` and sees an empty
  `mcpServers`, even though `claude mcp list` reports the servers `Connected` (that command appears
  to resolve/normalize case, session startup's tool-loading does not). Confirmed 2026-07-08: found
  two sibling project entries, one `C:/Users/Shiloh/Code/life-os` holding
  `liberado-deliberate-mcp`/`liberado-pr-dispatch-mcp`, one `c:/Users/Shiloh/Code/life-os` empty.
  Fixed by copying `mcpServers` into the lowercase-key entry (backed up `.claude.json` first). Even
  after the config fix, the *already-running* session still needs a restart â€” this is a second,
  independent cause of "MCP tools not visible" on Windows, not just the restart-timing one above.
- **GitHub has no PR-deletion mechanism**, even for repo owners on private repos â€” only closing.
  Distinct from issues (which admins can delete). Don't promise otherwise.
- **SSE event naming**: the error event is named **`failed`**, not `error` â€” browser `EventSource`
  reserves `error` for its own connection errors.
- **MCP server child processes have zero filesystem sandboxing** (confirmed:
  `turbomcp/crates/turbomcp-transport/src/child_process.rs` sets no `current_dir`/env restriction on
  spawned processes). A co-resident MCP process can write directly to the vault â€” why writer-identity
  verification (hardening audit item 1) needs OS-level isolation, not a code patch.
- **WASM builds need the rustup toolchain, not the standalone Rust install on PATH** â€” full
  explanation: the [`webui` README](../../../../crates/webui/README.md#build-commands).

---

## Live constraints (must not violate)

- **Never print or echo `DEEPSEEK_API_KEY`, `OPENROUTER_API_KEY`, or any secret/token/PAT** â€” confirm
  only length/prefix.
- `turbovault` / `turbomcp` PR branches push to the **`ForrestThump` fork only** â€” no upstream PRs
  without explicit permission. (The `vtcode` fork PR is a documented, one-off exception â€” the user
  explicitly directed forking, fixing, and opening it themselves.)
- **Outward-facing actions need confirmation.**
- Don't commit, push, or run servers/daemons without being asked. (Starting `turbovault` and the
  PR-dispatch server this session was in direct service of an explicit request to stand up the
  deliberation/dispatch tooling â€” still worth re-confirming before doing it reflexively elsewhere.)
- **Never amend a prior commit** unless explicitly asked â€” create new commits.
- **Stage explicit file lists**, not `git add -A`/`-u`.
- **`liberado-pr-dispatch-mcp`'s six Windows-portability fixes are uncommitted** â€” don't let a future
  session assume they're already shipped; check `git status` in that repo first.
