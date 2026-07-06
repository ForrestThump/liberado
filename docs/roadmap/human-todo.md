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

## Standing category: GUI verification

I don't have a way to visually drive a browser or a real terminal UI in this environment. Whenever
a change could plausibly affect the WebUI or TUI, someone needs to actually click through the real
pages / actual TUI screens — passing tests and a clean build confirm the code is correct, not that
it *renders* correctly. I'll flag it explicitly here whenever a specific change needs this, rather
than assuming it's covered.
