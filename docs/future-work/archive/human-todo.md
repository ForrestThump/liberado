# Human to-do â€” things only you can do

A living checklist: setup steps, credentials, and manual verification I can't do myself (either
because it needs a secret only you should hold, needs a real running service started on your
machine, or needs eyes on an actual rendered UI). I'll keep this updated as work continues â€” check
things off or delete them once done; add a dated note if something's blocked on you for a while.

## Wake-up scheduler (`liberado-wakeup-mcp`) â€” live end-to-end, confirmed 2026-07-07

Full pipeline verified for real: `schedule_wakeup` (driven manually over stdio) â†’ `wakeup-poller`
picked up the due note, POSTed to the daemon's webhook, deleted the note â†’ daemon's own
`/api/reactions` log shows `WebhookFired` with `outcome: "acted"`. Env vars, the standalone
`turbovault` instance, and `wakeup-poller` are all confirmed working together against your real
vault and real daemon.

- [x] `WAKEUP_HOOK_SECRET` / `LIFEOS_HOOK_SECRET` set (matching values).
- [x] Standalone `turbovault` running with HTTP transport against the real vault:
      ```
      cd turbovault
      cargo run --package turbovault --features http --bin turbovault -- \
          --transport http --port 3737 --vault "C:/Users/Shiloh/Obsidian/Main" --init
      ```
      No process supervision yet (no systemd unit, no Docker wiring for HTTP mode) â€” still a manual
      `cargo run`, and confirmed **not** to survive a machine restart (went down in the power-loss
      incident this session). Let me know if you want this turned into something that survives a
      reboot â€” didn't build it since I didn't want to guess at your actual deployment setup
      (systemd? Task Scheduler? Docker?).
- [x] `wakeup-poller` run and confirmed firing real wake-ups.
- [ ] Point your MCP client at `liberado-wakeup-mcp/target/release/wakeup-mcp` (stdio) wherever you
      want `schedule_wakeup`/`cancel_wakeup`/`list_wakeups` available day-to-day â€” so far only
      driven manually (a hand-crafted `initialize`/`tools/call` sequence piped into its stdin) for
      verification, not wired into an actual MCP client. Not yet wired into life-os's own
      `topology.toml` `[[mcps]]` list either; tell me if/how you want that done (I'll need your
      input on the `consequence` rating, since that's deliberately not something an MCP or I
      self-declare).

**Known cosmetic issue**: the poller's log showed a few `ERROR turbomcp_http::transport: Error
reading SSE stream` lines during the live test. Traced to `turbomcp-client`'s background, optional
GET `/sse` push-notification listener â€” a separate code path from the actual `search_by_frontmatter`/
`read_note`/`delete_note` calls this repo uses, and it retries independently with its own backoff.
The real fire-and-delete cycle succeeded despite these, confirming they're noise, not a functional
problem (this client never subscribes to server-push notifications in the first place). Worth
quieting eventually â€” not urgent.

## `liberado-deliberate-mcp` fully decoupled from life-os â€” 2026-07-06

Migrated off all five life-os path deps (`liberado-provider`, `liberado-provider-openai-compat`,
`liberado-common`, `liberado-vault`, `liberado-config`) plus the local `turbomcp` path dep â€” same
"standalone repo talking to a standalone `turbovault` HTTP server" pattern as `liberado-wakeup-mcp`,
plus a self-contained OpenAI-compatible provider client replacing `liberado-provider`. Live-verified
this session against your real vault and real OpenRouter-routed participants: `initiate_deliberation`
on one server instance wrote a real note, `resume_deliberation` on a second, unrelated server
instance read it back correctly (`server::write_then_resume_round_trips_through_a_live_turbovault`,
`#[ignore]`-gated like the existing smoke test â€” the test note itself was deleted afterward).

- [x] Real config migrated: `life-os/config/deliberation.toml` â†’ copied to
      `C:\Users\Shiloh\AppData\Roaming\liberado-deliberate-mcp\deliberation.toml` (the platform
      config dir `dirs::config_dir()` resolves to, since this repo no longer shares life-os's
      `config/` directory), with `vault_path` replaced by `turbovault_url = "http://127.0.0.1:3737"`
      â€” the same standalone `turbovault` instance `liberado-wakeup-mcp` uses, no reason to run two.
- [ ] `life-os/config/deliberation.toml` (the old copy, with `vault_path`) is now stale/superseded â€”
      delete it whenever you're comfortable; I left it alone since it's your file, not mine to
      remove without asking.
- [ ] The `turbovault` instance I started for this session's live tests was a plain foreground
      `cargo run` â€” confirmed it does *not* survive a restart (it went down when this machine lost
      power mid-session). It'll need restarting (see the command above) before either MCP can be
      used live again. Same open question as above: let me know if you want a persistent-process
      story (systemd? Task Scheduler? Docker?) for the one `turbovault` instance both MCPs share.
- [x] **Point your MCP client at it â€” done for this Claude Code project, 2026-07-08**, via
      `claude mcp add liberado-deliberate-mcp --scope local -- <path to the .exe>` (local-scoped, so
      it's saved in this project's `.claude.json` entry, not global). A fresh `turbovault` was
      started against your real vault (`C:/Users/Shiloh/Obsidian/Main`) first â€” same "does not
      survive a restart" caveat as above; PID found still alive and healthy from an earlier attempt
      in this same session, so no restart was actually needed this time. **Not yet wired into
      life-os's own `topology.toml`** â€” this registration is Claude-Code-side only, for using the
      deliberation tools *from this chat*, which is a distinct decision from the `mcp-forge`/
      `topology.toml` question in the section below. New MCP servers only load into a running
      Claude Code session at startup, so **this session needs a restart/reload** before the
      registered tools (`initiate_deliberation`, `run_deliberation_round`, etc.) actually appear â€”
      confirmed via `claude mcp list` showing both as `Connected` already.

## Shared crates extracted: `liberado-standalone-kit`

`TurbovaultClient`/`content_hash` and the frontmatter-fence convention turned up duplicated,
near-identically, in both `liberado-wakeup-mcp` and `liberado-deliberate-mcp` (and a third time in
life-os's own `crates/common`); the OpenAI-compatible provider client was only in
`liberado-deliberate-mcp`, but was equally generic. All three moved to a new repo,
[liberado-standalone-kit](https://github.com/ForrestThump/liberado-standalone-kit) (private, three
crates: `frontmatter-note`, `turbovault-client`, `openai-compat-client`), consumed via a pinned
`git = "ssh://...", rev = "..."` dependency â€” the same pattern already used for the `turbomcp` fork,
not crates.io (would require the crate going public; deferred until an outside project actually
wants to depend on one of these). Both `liberado-wakeup-mcp` and `liberado-deliberate-mcp` are
migrated and live-verified (all existing tests plus both live-turbovault smoke tests pass
unchanged). `crates/common/src/frontmatter.rs` in life-os itself is untouched â€” it's not trying to
be portable, no reason to add an external git dependency to the main daemon for it.

- [ ] Nothing required of you right now â€” informational. One new fact worth knowing: building
      either MCP repo (directly, or via `mcp-forge`) now requires SSH access to
      `liberado-standalone-kit` on whatever machine does the build, plus
      `net.git-fetch-with-cli = true` in that machine's `~/.cargo/config.toml` (already set on this
      one â€” libgit2's own SSH implementation doesn't pick up `ssh-agent` the way the system `git`
      binary does, which is what broke the very first attempt to resolve this dependency).
- [ ] No semver yet on the shared crates â€” bumping one means manually updating the pinned `rev` in
      each consumer's `Cargo.toml` (currently 2 files: `liberado-wakeup-mcp`, `liberado-deliberate-mcp`).
      Fine at this scale; worth automating (or moving to crates.io) if more consumers show up.

## `mcp-forge` now supports `liberado-deliberate-mcp` â€” 2026-07-06

Verified: `liberado-mcp-forge` can now build `liberado-deliberate-mcp` via a `path` source (no more
manual `cargo build --bin` + hand-typed `command` path in `topology.toml`). Not yet wired in for
real â€” this is a small decision + config change, not something requiring new code:

- [ ] Decide whether to register `liberado-deliberate-mcp` as a real `[[mcps]]` entry
      (`transport = { kind = "managed" }`) plus a `[[source]]` in `mcp-sources.toml`
      (`path = "../liberado-deliberate-mcp"`). If yes, I need your input on the `consequence`
      rating (deliberately human-owned, not something I self-declare).

## Phase 4: Docker MCP transport â€” needs a live smoke test â€” 2026-07-07

Built `McpTransport::Docker` (a config-driven way to run an MCP server inside a container instead of
directly as a host process â€” isolation for a less-trusted or freshly-scaffolded MCP) â€” full design in
[phase-4-docker-transport.md](phase-4-docker-transport.md). Everything that can be verified without
a running Docker daemon is done: `cargo build --workspace`/`cargo clippy --all-targets` clean, all
new unit tests passing (config round-trip, validation, `docker_argv`, registry registration).

- [ ] **Live end-to-end smoke test still needed** â€” checked, and Docker Desktop's CLI is installed
      on this machine but its daemon isn't running (`docker version` connects fine to the client,
      but fails to reach `npipe:////./pipe/dockerDesktopLinuxEngine`). Once Docker Desktop is
      started:
      1. Wrap `tasks-mcp` in a throwaway Dockerfile: `FROM node:22-slim` /
         `CMD ["npx", "--yes", "@liberado/tasks-mcp"]`, then
         `docker build -t liberado-tasks-mcp:docker-test .`
      2. Add a `[[mcps]]` entry using the commented example already in
         `config.example/topology.toml` (`transport = { kind = "docker", image =
         "liberado-tasks-mcp:docker-test" }`).
      3. Confirm the daemon's live MCP registry shows the same tools as the existing
         `npx`-based `tasks-mcp` entry â€” proves the full path (config parse â†’ `docker_argv` â†’
         `StdioConnector` â†’ `docker run -i --rm` â†’ MCP handshake â†’ `list_tools()`) works for real,
         not just that it compiles.
- [ ] This dev machine is Windows (Docker Desktop/WSL2); the actual deployment target is Debian,
      where none of the Windows-specific notes in `phase-4-docker-transport.md` (forward-slash
      volume paths, pre-pulling to avoid WSL2 VM-wake latency) apply â€” worth a second, real smoke
      test on the Debian target whenever that deployment happens, just to confirm nothing
      Windows-Docker-Desktop-specific snuck in unnoticed.

## `liberado-pr-dispatch-mcp` repo-context injection â€” 2026-07-07 (superseded in part, see below)

Built `util::read_repo_context` (reads whichever `AGENT_CONTEXT_PATHS` files exist in a task's
fresh clone â€” default list covers `CLAUDE.md`/`AGENTS.md`/`ARCHITECTURE.md`/
`docs/spec/architecture/overview.md`/`docs/impl/agents.md` â€” caps at `AGENT_CONTEXT_MAX_BYTES`)
and wired it into both the normal and revision coding prompts, merged with the task's own
`context` rather than replacing it. Full design + rationale in
`liberado-pr-dispatch-mcp/ARCHITECTURE.md`'s "Per-repo agent context" section (still under its old
`riggers/` name inside that doc's own text in places â€” not re-verified this pass). `cargo
build`/`cargo test`/`cargo clippy` all clean (4 pre-existing test failures in `vtcode_client`'s
tests are Windows-can't-spawn-a-Unix-shell-script issues, unrelated to this change).

> **Update, 2026-07-08**: the directory this describes is `liberado-pr-dispatch-mcp/` (renamed
> from `riggers/` 2026-07-06) and â€” contrary to the note below â€” **does** now have its own git repo
> with a GitHub remote (`git@github.com:ForrestThump/liberado-pr-dispatch-mcp.git`, one initial
> commit, 2026-07-06 22:52). The "no git repo of its own" concern below was accurate when written
> but no longer describes this checkout. The separate, real concern about a different
> homelab-deployed instance (Gitea, `riggers.yaml`, `services/agent-workspace`) is untouched by
> that fact and still stands as its own open item â€” I have no visibility into that deployment
> either way. See the new section below for what changed in this checkout since this entry was
> written, including a real bug found in `vtcode` itself along the way.

- [ ] **`riggers.yaml`'s homelab deployment is a separate instance from this checkout** â€” I have no
      visibility into its source/git history (your homelab's Gitea, most likely, given
      `riggers.yaml`'s own "Docker Compose homelab" framing and its `Dockerfile`'s
      `services/agent-workspace` build-context reference â€” that path suggests a different directory
      layout than this checkout). Porting this checkout's fixes there (both the repo-context
      injection above and the Windows-portability + `.vtcode`-scratch-dir fixes below) is still on
      you, whenever/if that homelab instance needs them.
- [ ] **Confirmed real, still true**: `liberado-pr-dispatch-mcp/Cargo.toml` has `liberado-provider =
      { path = "../crates/provider" }` â€” a literal path dependency on life-os's own crate. Building
      it (Docker or otherwise) requires life-os's `crates/provider` available at that exact relative
      path at build time. Worth confirming the homelab deployment's build process actually satisfies
      this (vendors/copies the crate in, or checks out life-os alongside it) â€” I can't verify this
      from here since I don't know that deployment's actual layout.
- [x] **Smoke-test task against `ForrestThump/liberado` â€” done, clean, 2026-07-08.** See the new
      section directly below for the full story (it took two attempts and surfaced a real upstream
      `vtcode` bug along the way). Result:
      [ForrestThump/liberado#2](https://github.com/ForrestThump/liberado/pull/2) â€” draft, single-line
      diff (`chat.rs` `title` attribute), awaiting your review/approval. **Not merged â€” I never
      merge draft PRs myself.**

## `liberado-pr-dispatch-mcp` Windows portability pass + a real upstream `vtcode` bug â€” 2026-07-08

Goal was to prove the PR-dispatch pipeline actually works end-to-end on this Windows dev machine
before "shotgunning" a batch of WebUI tasks through it. It didn't work on the first try, and every
failure was real, not environmental noise:

- **Fixed in `liberado-pr-dispatch-mcp`** (committed locally; see checkpoint notes below): hardcoded Unix paths
  (`/data/tasks.db`, `/workspace`) now read `DB_PATH`/`WORKSPACE_DIR`/`BIND_ADDR` env vars; a
  Windows `git-askpass` helper was added (`git_ops.rs` previously hard-bailed on non-Unix); Windows'
  Git Credential Manager was intercepting auth before `GIT_ASKPASS` was consulted and hanging
  non-interactively, fixed with `-c credential.helper=` on the specific git invocations; the coding
  prompt is now piped over stdin instead of passed as a CLI arg (Windows' ~32K command-line length
  limit was truncating it); a TOML double-quoted string in the generated per-task vtcode config was
  breaking on Windows path backslashes, fixed with a TOML literal (single-quoted) string; and
  vtcode's own `.vtcode/` scratch directory was leaking into task commits (a bad draft PR, #1 on
  `ForrestThump/liberado`, closed + branch deleted once diagnosed) â€” `commit_pending` now removes it
  first.
- **Found and fixed a genuine bug in upstream `vtcode`** (not this repo, not a config mistake â€”
  verified by reading the actual source across three versions before concluding it was real):
  `build_primary_agent_runtime_config` in `vtcode-core/src/primary_agent.rs` unconditionally
  overwrote the resolved model with the literal string `"inherit"` (the sentinel built-in primary
  agents default `model` to), instead of falling back to the parent's real model the way the
  analogous `resolve_subagent_model` already correctly does. Every plain `exec` run failed with
  `Model 'inherit' is not recognized`. Fixed on a fork
  ([ForrestThump/VTCode](https://github.com/ForrestThump/VTCode), branch
  `fix/primary-agent-inherit-model`), 28/28 `vtcode-core` tests pass, `cargo fmt`/`clippy` clean.
  **PR opened upstream**: [vinhnx/VTCode#697](https://github.com/vinhnx/VTCode/pull/697) â€” you
  opened it yourself 2026-07-08; awaiting the maintainer's review/merge.
- End-to-end smoke test then succeeded cleanly against the patched fork binary:
  [ForrestThump/liberado#2](https://github.com/ForrestThump/liberado/pull/2), a single-line diff
  adding a `title="Send (Enter)"` tooltip to the chat send button â€” exactly the intended change,
  nothing else touched.

- [x] **The six Windows-portability fixes committed â€” 2026-07-08** (`5157b21`, local only, not
      pushed â€” say the word if you want it on `origin/master`).
- [x] **Follow-up PR-dispatch iteration checkpoint committed locally â€” 2026-07-09** (`32e5815`,
      local only, not pushed): the vtcode cleanup/coder-critic retry-loop work, `config.toml`, and
      `sessions/` diagnostics were committed as-is before the Rust-native coder planning pass, per
      the user's request for a clean working tree. The files are diagnostic/scratch state rather than
      secrets, but they are now part of the local repo history.
- [x] **`vtcode` fork PR merged upstream â€” 2026-07-08.**
      [vinhnx/VTCode#697](https://github.com/vinhnx/VTCode/pull/697) merged into `main` at `fe45c4e`
      (confirmed via `gh pr view`). No tagged release includes it yet as of this writing (latest is
      `0.134.14`, published *before* the merge) â€” cleaned up
      `liberado-pr-dispatch-mcp` accordingly: `Dockerfile` reverted from the temporary
      `cargo install vtcode` (crates.io) workaround back to building from source, now pointing at
      upstream `vinhnx/VTCode` (not the `ForrestThump` fork) tracking `main` by default
      (`VTCODE_REF`, still overridable to pin a tag/commit later). `vtcode.toml`'s `[subagents]` are
      re-enabled (`enabled = true`) now that the fix that required disabling them is upstream. The
      `ForrestThump/VTCode` fork itself is no longer needed by this checkout â€” safe to archive or
      delete whenever you want, not urgent.
- [ ] **`ForrestThump/liberado` PR #2 is a draft, awaiting your review/approval** â€” I deliberately
      did not approve or merge it myself.
- [x] **`liberado-pr-dispatch-mcp` also registered as an MCP server for this Claude Code project â€”
      2026-07-08**, via `claude mcp add --transport http liberado-pr-dispatch-mcp
      http://127.0.0.1:8000/mcp --header "Authorization: Bearer <token>" --scope local`, pointing at
      the already-running smoke-test dispatch server (same instance PR #2 came from â€” found still
      alive from earlier in this session; a second instance I tried to start hit "port already in
      use" against it, confirming it survived). Same restart caveat as `liberado-deliberate-mcp`
      above: registered and `Connected` per `claude mcp list`, but this session needs a
      restart/reload before `submit_pr_factory_task`/`get_pr_status` actually appear as usable
      tools. **This dispatch server is the scratch smoke-test instance** (`REPOS_CONFIG`/
      `DISPATCH_CONFIG` pointed at a temp scratchpad dir, `VTCODE_BIN` pointed at the patched fork
      binary) â€” not a persistent, reboot-surviving deployment. The 10-task WebUI batch (2026-07-08)
      was submitted through this same scratch instance, still on the fork binary â€” functionally fine
      since the fork *has* the fix, just no longer necessary now that it's upstream. Worth deciding,
      once you're ready to actually "shotgun" further batches, whether to keep using this scratch
      instance or stand up a real one; if/when you do rebuild it, it can point `VTCODE_BIN` at a
      binary built from upstream `vinhnx/VTCode` `main` instead of the fork.

## Standing category: GUI verification

I don't have a way to visually drive a browser or a real terminal UI in this environment. Whenever
a change could plausibly affect the WebUI or TUI, someone needs to actually click through the real
pages / actual TUI screens â€” passing tests and a clean build confirm the code is correct, not that
it *renders* correctly. I'll flag it explicitly here whenever a specific change needs this, rather
than assuming it's covered.
