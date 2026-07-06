# Human to-do — things only you can do

A living checklist: setup steps, credentials, and manual verification I can't do myself (either
because it needs a secret only you should hold, needs a real running service started on your
machine, or needs eyes on an actual rendered UI). I'll keep this updated as work continues — check
things off or delete them once done; add a dated note if something's blocked on you for a while.

## Wake-up scheduler (`liberado-wakeup-mcp`) — 2026-07-06

- [ ] Set two environment variables to the **same** secret value: `WAKEUP_HOOK_SECRET` (read by
      the life-os daemon, resolves the `wakeup-fired` hook in `config/topology.toml`) and
      `LIFEOS_HOOK_SECRET` (read by `wakeup-poller`). Pick any random string; they just need to
      match each other.
- [ ] Start a standalone `turbovault` instance with HTTP transport, pointed at your real vault —
      this is a separate, long-running process from the life-os daemon:
      ```
      cd turbovault
      cargo run --package turbovault --features http --bin turbovault -- \
          --transport http --port 3737 --vault "C:/Users/Shiloh/Obsidian/Main" --init
      ```
      No process supervision exists for this yet (no systemd unit, no Docker wiring for HTTP mode —
      see `liberado-wakeup-mcp/ARCHITECTURE.md`) — it's a manual `cargo run` for now. Let me know if
      you want that turned into something that survives a reboot; I didn't build it since I didn't
      want to guess at your actual deployment setup (systemd? Task Scheduler? Docker?).
- [ ] Run `wakeup-poller` as its own long-running process, with `TURBOVAULT_URL`,
      `LIFEOS_WEBHOOK_URL` (e.g. `http://127.0.0.1:8080/api/hooks/wakeup-fired` — check the real
      port your life-os server binds), and `LIFEOS_HOOK_SECRET` set.
- [ ] Point your MCP client at `liberado-wakeup-mcp/target/release/wakeup-mcp` (stdio) wherever you
      want `schedule_wakeup`/`cancel_wakeup`/`list_wakeups` available — I haven't wired it into
      life-os's own `topology.toml` `[[mcps]]` list yet; tell me if/how you want that done (I'll
      need your input on the `consequence` rating, since that's deliberately not something an MCP
      or I self-declare).
- [ ] Create an empty repo on your Gitea for `liberado-wakeup-mcp` and share the URL — it's
      committed locally only so far.

## Full live end-to-end test (not yet run)

- [ ] With all of the above running: schedule a real wake-up a minute or two out, confirm it
      actually fires as a new dispatched task once it's due. I verified every piece against a
      scratch vault + a mock webhook receiver, but haven't run the real path against your real
      daemon/vault — that's a "start new services against real infrastructure" step I held off on
      without your go-ahead.

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
- [ ] The `turbovault` instance I started for this session's live test is a plain foreground
      `cargo run`, same as the wake-up scheduler's — it may not survive this session ending. Same
      open question as above: let me know if you want a persistent-process story (systemd? Task
      Scheduler? Docker?) for the one `turbovault` instance both MCPs now share.
- [ ] Point your MCP client at `liberado-deliberate-mcp/target/release/liberado-deliberate-mcp`
      (stdio) wherever you want the deliberation tools available — not yet wired into life-os's own
      `topology.toml` `[[mcps]]` list (see the `mcp-forge` section below, which covers exactly that
      decision).

## `mcp-forge` now supports `liberado-deliberate-mcp` — 2026-07-06

Verified: `liberado-mcp-forge` can now build `liberado-deliberate-mcp` via a `path` source (no more
manual `cargo build --bin` + hand-typed `command` path in `topology.toml`). Not yet wired in for
real — this is a small decision + config change, not something requiring new code:

- [ ] Decide whether to register `liberado-deliberate-mcp` as a real `[[mcps]]` entry
      (`transport = { kind = "managed" }`) plus a `[[source]]` in `mcp-sources.toml`
      (`path = "../liberado-deliberate-mcp"`). If yes, I need your input on the `consequence`
      rating (deliberately human-owned, not something I self-declare).

## Standing category: GUI verification

I don't have a way to visually drive a browser or a real terminal UI in this environment. Whenever
a change could plausibly affect the WebUI or TUI, someone needs to actually click through the real
pages / actual TUI screens — passing tests and a clean build confirm the code is correct, not that
it *renders* correctly. I'll flag it explicitly here whenever a specific change needs this, rather
than assuming it's covered.
