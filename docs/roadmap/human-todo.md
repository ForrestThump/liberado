# Human to-do — things only you can do

A living checklist: setup steps, credentials, and manual verification I can't do myself (either
because it needs a secret only you should hold, needs a real running service started on your
machine, or needs eyes on an actual rendered UI). I'll keep this updated as work continues — check
things off or delete them once done; add a dated note if something's blocked on you for a while.

## Wake-up scheduler (`liberado-wakeup-mcp`) — live end-to-end, confirmed 2026-07-07

Full pipeline verified for real: `schedule_wakeup` (driven manually over stdio) → `wakeup-poller`
picked up the due note, POSTed to the daemon's webhook, deleted the note → daemon's own
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
      No process supervision yet (no systemd unit, no Docker wiring for HTTP mode) — still a manual
      `cargo run`, and confirmed **not** to survive a machine restart (went down in the power-loss
      incident this session). Let me know if you want this turned into something that survives a
      reboot — didn't build it since I didn't want to guess at your actual deployment setup
      (systemd? Task Scheduler? Docker?).
- [x] `wakeup-poller` run and confirmed firing real wake-ups.
- [ ] Point your MCP client at `liberado-wakeup-mcp/target/release/wakeup-mcp` (stdio) wherever you
      want `schedule_wakeup`/`cancel_wakeup`/`list_wakeups` available day-to-day — so far only
      driven manually (a hand-crafted `initialize`/`tools/call` sequence piped into its stdin) for
      verification, not wired into an actual MCP client. Not yet wired into life-os's own
      `topology.toml` `[[mcps]]` list either; tell me if/how you want that done (I'll need your
      input on the `consequence` rating, since that's deliberately not something an MCP or I
      self-declare).

**Known cosmetic issue**: the poller's log showed a few `ERROR turbomcp_http::transport: Error
reading SSE stream` lines during the live test. Traced to `turbomcp-client`'s background, optional
GET `/sse` push-notification listener — a separate code path from the actual `search_by_frontmatter`/
`read_note`/`delete_note` calls this repo uses, and it retries independently with its own backoff.
The real fire-and-delete cycle succeeded despite these, confirming they're noise, not a functional
problem (this client never subscribes to server-push notifications in the first place). Worth
quieting eventually — not urgent.

## `liberado-deliberate-mcp` fully decoupled from life-os — 2026-07-06

Migrated off all five life-os path deps (`liberado-provider`, `liberado-provider-openai-compat`,
`liberado-common`, `liberado-vault`, `liberado-config`) plus the local `turbomcp` path dep — same
"standalone repo talking to a standalone `turbovault` HTTP server" pattern as `liberado-wakeup-mcp`,
plus a self-contained OpenAI-compatible provider client replacing `liberado-provider`. Live-verified
this session against your real vault and real OpenRouter-routed participants: `initiate_deliberation`
on one server instance wrote a real note, `resume_deliberation` on a second, unrelated server
instance read it back correctly (`server::write_then_resume_round_trips_through_a_live_turbovault`,
`#[ignore]`-gated like the existing smoke test — the test note itself was deleted afterward).

- [x] Real config migrated: `life-os/config/deliberation.toml` → copied to
      `C:\Users\Shiloh\AppData\Roaming\liberado-deliberate-mcp\deliberation.toml` (the platform
      config dir `dirs::config_dir()` resolves to, since this repo no longer shares life-os's
      `config/` directory), with `vault_path` replaced by `turbovault_url = "http://127.0.0.1:3737"`
      — the same standalone `turbovault` instance `liberado-wakeup-mcp` uses, no reason to run two.
- [ ] `life-os/config/deliberation.toml` (the old copy, with `vault_path`) is now stale/superseded —
      delete it whenever you're comfortable; I left it alone since it's your file, not mine to
      remove without asking.
- [ ] The `turbovault` instance I started for this session's live tests was a plain foreground
      `cargo run` — confirmed it does *not* survive a restart (it went down when this machine lost
      power mid-session). It'll need restarting (see the command above) before either MCP can be
      used live again. Same open question as above: let me know if you want a persistent-process
      story (systemd? Task Scheduler? Docker?) for the one `turbovault` instance both MCPs share.
- [ ] Point your MCP client at `liberado-deliberate-mcp/target/release/liberado-deliberate-mcp`
      (stdio) wherever you want the deliberation tools available — not yet wired into life-os's own
      `topology.toml` `[[mcps]]` list (see the `mcp-forge` section below, which covers exactly that
      decision).

## Shared crates extracted: `liberado-standalone-kit`

`TurbovaultClient`/`content_hash` and the frontmatter-fence convention turned up duplicated,
near-identically, in both `liberado-wakeup-mcp` and `liberado-deliberate-mcp` (and a third time in
life-os's own `crates/common`); the OpenAI-compatible provider client was only in
`liberado-deliberate-mcp`, but was equally generic. All three moved to a new repo,
[liberado-standalone-kit](https://github.com/ForrestThump/liberado-standalone-kit) (private, three
crates: `frontmatter-note`, `turbovault-client`, `openai-compat-client`), consumed via a pinned
`git = "ssh://...", rev = "..."` dependency — the same pattern already used for the `turbomcp` fork,
not crates.io (would require the crate going public; deferred until an outside project actually
wants to depend on one of these). Both `liberado-wakeup-mcp` and `liberado-deliberate-mcp` are
migrated and live-verified (all existing tests plus both live-turbovault smoke tests pass
unchanged). `crates/common/src/frontmatter.rs` in life-os itself is untouched — it's not trying to
be portable, no reason to add an external git dependency to the main daemon for it.

- [ ] Nothing required of you right now — informational. One new fact worth knowing: building
      either MCP repo (directly, or via `mcp-forge`) now requires SSH access to
      `liberado-standalone-kit` on whatever machine does the build, plus
      `net.git-fetch-with-cli = true` in that machine's `~/.cargo/config.toml` (already set on this
      one — libgit2's own SSH implementation doesn't pick up `ssh-agent` the way the system `git`
      binary does, which is what broke the very first attempt to resolve this dependency).
- [ ] No semver yet on the shared crates — bumping one means manually updating the pinned `rev` in
      each consumer's `Cargo.toml` (currently 2 files: `liberado-wakeup-mcp`, `liberado-deliberate-mcp`).
      Fine at this scale; worth automating (or moving to crates.io) if more consumers show up.

## `mcp-forge` now supports `liberado-deliberate-mcp` — 2026-07-06

Verified: `liberado-mcp-forge` can now build `liberado-deliberate-mcp` via a `path` source (no more
manual `cargo build --bin` + hand-typed `command` path in `topology.toml`). Not yet wired in for
real — this is a small decision + config change, not something requiring new code:

- [ ] Decide whether to register `liberado-deliberate-mcp` as a real `[[mcps]]` entry
      (`transport = { kind = "managed" }`) plus a `[[source]]` in `mcp-sources.toml`
      (`path = "../liberado-deliberate-mcp"`). If yes, I need your input on the `consequence`
      rating (deliberately human-owned, not something I self-declare).

## Phase 4: Docker MCP transport — needs a live smoke test — 2026-07-07

Built `McpTransport::Docker` (a config-driven way to run an MCP server inside a container instead of
directly as a host process — isolation for a less-trusted or freshly-scaffolded MCP) — full design in
[phase-4-docker-transport.md](phase-4-docker-transport.md). Everything that can be verified without
a running Docker daemon is done: `cargo build --workspace`/`cargo clippy --all-targets` clean, all
new unit tests passing (config round-trip, validation, `docker_argv`, registry registration).

- [ ] **Live end-to-end smoke test still needed** — checked, and Docker Desktop's CLI is installed
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
         `npx`-based `tasks-mcp` entry — proves the full path (config parse → `docker_argv` →
         `StdioConnector` → `docker run -i --rm` → MCP handshake → `list_tools()`) works for real,
         not just that it compiles.
- [ ] This dev machine is Windows (Docker Desktop/WSL2); the actual deployment target is Debian,
      where none of the Windows-specific notes in `phase-4-docker-transport.md` (forward-slash
      volume paths, pre-pulling to avoid WSL2 VM-wake latency) apply — worth a second, real smoke
      test on the Debian target whenever that deployment happens, just to confirm nothing
      Windows-Docker-Desktop-specific snuck in unnoticed.

## `riggers` repo-context injection — important: not committed anywhere yet — 2026-07-07

Built `util::read_repo_context` (reads whichever `AGENT_CONTEXT_PATHS` files exist in a task's
fresh clone — default list covers `CLAUDE.md`/`AGENTS.md`/`ARCHITECTURE.md`/
`docs/architecture/overview.md`/`docs/contributing/agents.md` — caps at `AGENT_CONTEXT_MAX_BYTES`)
and wired it into both the normal and revision coding prompts, merged with the task's own
`context` rather than replacing it. Full design + rationale in `riggers/ARCHITECTURE.md`'s new
"Per-repo agent context" section. `cargo build`/`cargo test`/`cargo clippy` all clean (4
pre-existing test failures in `vtcode_client`'s tests are Windows-can't-spawn-a-Unix-shell-script
issues, unrelated to this change and not something I introduced).

- [ ] **Important — `riggers/` has no git repo of its own in this checkout.** It's a plain,
      `.gitignore`d directory inside the life-os working tree (confirmed: no `.git` inside it, and
      it's listed in life-os's own `.gitignore`). My edits are real files on disk at
      `riggers/src/util.rs`, `riggers/src/config.rs`, `riggers/src/worker.rs`,
      `riggers/.env.example`, `riggers/ARCHITECTURE.md`, `riggers/repos.toml` — but there's no
      commit anywhere, and I have no visibility into wherever the *actual* deployed riggers
      instance's source/git history lives (your homelab's Gitea, most likely, given
      `riggers.yaml`'s own "Docker Compose homelab" framing and its `Dockerfile`'s
      `services/agent-workspace` build-context reference — that path suggests the real deployment
      repo has a different directory layout than this checkout). You'll need to port these six
      files' changes into wherever that real repo actually is before they take effect.
- [ ] **Confirmed real, still true**: `riggers/Cargo.toml` has `liberado-provider = { path =
      "../crates/provider" }` — a literal path dependency on life-os's own crate. Building riggers
      (Docker or otherwise) requires life-os's `crates/provider` available at that exact relative
      path at build time. Worth confirming the real deployment's build process actually satisfies
      this (vendors/copies the crate in, or checks out life-os alongside it) — I can't verify this
      from here since I don't know that deployment's actual layout.
- [ ] **To target `liberado` without touching the homelab's working primary-repo config**: add it
      as an *additional* repo, not the primary (which stays on `GITEA_URL`/`GITEA_REPO`/
      `GITEA_TOKEN` env vars, untouched). A commented example is already in `riggers/repos.toml`:
      ```toml
      [[repos]]
      slug = "ForrestThump/liberado"
      url = "https://github.com"
      token = "ghp_..."
      provider = "github"
      ```
      Needs a **new GitHub PAT** scoped to just this repo (contents + pull-requests write is
      enough for a fine-grained token). If `ALLOWED_REPOS` is set in whatever's actually deployed,
      add `ForrestThump/liberado` to that allowlist too, or it'll be rejected even though it's in
      `repos.toml`.
- [ ] **Recommend one small, real smoke-test task before the 10-task shotgun** — dispatch something
      trivial and easily-reversible against `ForrestThump/liberado` first (e.g. "fix a typo in a
      doc comment") and actually look at the resulting draft PR: does the repo context show up in
      vtcode's behavior (does it follow this project's actual conventions), does the PR land
      against `main` (not `develop` — `riggers.yaml`'s `default_base_branch: develop` is the
      homelab's own default; override per-task via the task's `target_branch` field, e.g.
      `"main"`, rather than changing the global default and risking the homelab's own tasks), does
      auth/push/PR-creation work end-to-end. Cheap insurance before committing to volume.

## Standing category: GUI verification

I don't have a way to visually drive a browser or a real terminal UI in this environment. Whenever
a change could plausibly affect the WebUI or TUI, someone needs to actually click through the real
pages / actual TUI screens — passing tests and a clean build confirm the code is correct, not that
it *renders* correctly. I'll flag it explicitly here whenever a specific change needs this, rather
than assuming it's covered.
