# liberado-webui

The Dioxus (WASM) web UI for Liberado. It is a **pure frontend**: it renders in the
browser and talks to the **daemon's HTTP API on port 4201**. It does not embed any
agent logic — every capability is a call to the daemon.

This README is the **handoff doc** for iterating on the UI. It describes how the app
is wired, the styling system, the dev loop, and the known gotchas, so you can start
making changes without re-deriving any of it.

---

## TL;DR — run it

```bash
# Copy config.example/ops.toml to .liberado/ops.toml first.
just dev-start --vault <path-to-vault>
just webui-dev

just stop-webui-dev
just stop-daemon
```

- **Edit the UI against `:8080`** (hot reload). The WASM there still calls the daemon's
  API on `:4201` (see [api_base](#api-base--cors)), so the daemon must be running too.
- **`:4201` serves a *static release build*** of this crate. It only updates when you
  rebuild the bundle (`dx build` below). If
  `:4201` looks stale, that's why — hard-refresh after a rebuild, or just use `:8080`.

### Ship it to the homelab

```bash
just deploy-webui-homelab
```

The bundle is **mounted** into the daemon's container rather than baked into its image, and
`ServeDir` re-reads it per request — so this is a file copy, with no image rebuild and no restart.
Do *not* use `just deploy-homelab` for a frontend-only change; that rebuilds the daemon image
and takes 20–40 minutes. See `deploy/homelab/README.md`.

---

## Build commands

`dx` is the Dioxus CLI. **It must run with the rustup-managed cargo**, which has the
`wasm32-unknown-unknown` std — the standalone Rust on `PATH` does not (see
[Gotchas](#gotchas)). The scripts handle this; if you run `dx` by hand:

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
dx serve -p liberado-webui --platform web --addr 0.0.0.0 --port 8080   # dev / hot reload
dx build -r -p liberado-webui --web                                    # release bundle
```

The release bundle lands in `target/dx/liberado-webui/release/web/public`, which the
daemon serves via `ServeDir` (constant `DIST_DIR` in `crates/server/src/lib.rs`).

---

## Architecture

```
crates/webui/src/
  main.rs                  App shell: header + nav, view switch (Chat | Status),
                           theme-CSS injection, api_base() origin detection.
  theme.rs                 theme_css_vars(&Theme) -> ":root { --lib-*: … }" block.
  styles/main.css          All styling. Semantic classes over var(--lib-*) tokens.
  components/
    mod.rs                 Module list (chat, dashboard, reactions, vault).
    chat.rs                Chat view: message list + input, SSE streaming.
    dashboard.rs           Status view: status banner + vault + reactions panels.
    reactions.rs           Recent reactions panel.
    vault.rs               Vault info panel.
    slash_commands.rs      *Scaffolded, NOT wired in* — see "Slash commands" below.
```

### Data flow
- `main.rs::api_base()` computes the daemon base URL once and passes it to each view.
- **Dashboard** fetches `GET /api/status` (and the panels hit `/api/vault`,
  `/api/reactions`) with `reqwest`, deserializing into types from the
  **`chat-client-contract`** crate (the shared wire DTOs — single source of truth,
  reused by the TUI and CLI too).
- **Chat** streams a turn over SSE. The wire events (`session`, `token`, `tool`,
  `tool_result`, `done`, `failed`) are parsed via
  `chat_client_contract::ChatEvent::from_sse_data(event_type, data)`.

---

## Styling system (read this before touching CSS)

No Tailwind, no CSS-in-JS, no build step for styles. Two layers:

1. **Theme tokens → CSS custom properties.** `crates/theme` (`liberado-theme`) defines
   a flat `Theme` struct of color tokens. `theme.rs::theme_css_vars()` turns a `Theme`
   into a `:root { --lib-accent: …; --lib-app-bg: …; … }` block. `main.rs` injects that
   block in a `<style>` **before** `main.css`:

   ```rust
   style { {crate::theme::theme_css_vars(&liberado_theme::Theme::default_dark())} }
   style { {include_str!("./styles/main.css")} }
   ```

   Swapping the `Theme` argument is all it takes to re-theme the whole app. Two
   structural surfaces are *derived* from existing tokens (not new theme fields):
   `--lib-surface` (panel / assistant-bubble bg) and `--lib-surface-2` (code / tool-chip bg).

2. **Semantic classes in `main.css`** that reference `var(--lib-*)`. Components only
   ever set class names (`class: "bubble user"`), never inline colors. To restyle,
   edit `main.css`; to recolor, edit the theme token (or add one and map it in `theme.rs`).

**CSS authoring constraint:** the stylesheet is injected as the text content of a
`<style>` element, and Dioxus HTML-escapes text. So **do not use `<`, `>`, or `&` in
`main.css`** — that means no `>` child combinators and no `&` nesting. Use descendant
selectors (`.card-header h2`) and `+` sibling selectors (both fine). The current file
already follows this; keep it that way or the rules silently break.

### Class inventory (already defined in `main.css`)
App shell (`.app`, `.app-header`, `.brand`, `.nav`, `.nav-btn`), chat
(`.chat`, `.messages`, `.bubble-row`, `.bubble.{user,assistant,system,tool,error}`,
`.bubble-thinking`, `.input-bar`, `.input`, `.send-btn`, `.empty-state`), dashboard
(`.dashboard`, `.card`, `.status-banner`, `.status-dot`, `.stat-tile`, `.vault-row`,
`.reaction-row`, `.spinner`, `.error-card`). Reuse these before inventing new ones.

---

## api_base & CORS

`main.rs::api_base()` (wasm build) is **same-origin by default** — it returns
`window.location.origin`, because whoever served the page also answers `/api/*`:

| Served by | Page origin | API base |
|---|---|---|
| the daemon, directly | `http://<host>:4201` | same |
| Traefik → the daemon | `https://liberado.homelab.local` | same |
| `dx serve` (dev) | `http://<host>:8080` | `http://<host>:4201` |

`dx serve` is the one exception: it can't proxy, so port `8080` — and only `8080` — retargets the
daemon. That cross-port case is what `CorsLayer::permissive()` on the daemon exists for; the two
same-origin cases need no CORS at all.

**Don't reintroduce a hardcoded `:4201`.** It was one, and it broke the homelab deploy: at
`https://liberado.homelab.local/` the WASM asked for `https://liberado.homelab.local:4201`, where
Traefik does not listen (it terminates TLS on 443 only). The base must also stay **absolute** —
`reqwest`'s wasm client runs it through `Url::parse`, which rejects a relative path.

---

## Demo seed (remove when real history lands)

`chat.rs::demo_seed()` pre-populates the chat with a 5-message conversation so the
bubble/box styling is visible without a live daemon turn. It's marked
`TODO(handoff)` and is **purely a styling showcase** — replace it with real history
loaded from `GET /api/conversations/{id}` when that lands, or just drop it to start
from an empty chat (`use_signal(Vec::new)`).

---

## Slash commands (wired)

`components/slash_commands.rs` implements `CommandContext` over the chat state and exposes
`handle_slash_command()`, backed by the **`liberado-commands`** crate (the shared `/help`, `/new`,
`/theme`, … parser used by the TUI). It is in `mod.rs`, `liberado-commands` is a base dependency,
`ChatMsg` is `pub`, and `chat.rs::submit` calls it when `text` starts with `/`.

Both the call and the `CommandResult` match live inside `#[cfg(target_arch = "wasm32")]` blocks, so
the imports in `chat.rs` are gated the same way — ungate them and a native build fails the
workspace's zero-warnings bar on unused imports.

---

## UI roadmap (intended direction)

These are the agreed next steps for iterative UI work:

- **Sidebar** — settings + chat-history list (conversations from the daemon).
- **Markdown rendering** for assistant/user turns (via `pulldown-cmark`), with
  **copy buttons** on code blocks and on whole responses/prompts.
- **Slash commands** wired into the chat input (see section above).
- **[dioxus-primitives](https://github.com/DioxusLabs/components)** for headless,
  accessible building blocks (Tabs, Dialog, ScrollArea, Toast, …). crates.io currently
  has only a `0.0.0` placeholder, so depend on it via **git** (`DioxusLabs/components`).
  These are *unstyled* — they compose cleanly with the `--lib-*` token system: style
  them with our CSS via their data-attributes.

---

## Gotchas

- **wasm std / `dx`**: building for wasm fails with *"can't find crate for `core`"* if
  `dx` picks up the standalone Rust on `PATH` (it lacks the wasm32 target). Always
  prepend `$env:USERPROFILE\.cargo\bin` to `PATH` (the scripts do this). Verify with
  `rustup target list --installed` showing `wasm32-unknown-unknown`.
- **Stale `:4201`**: the daemon serves a *static* release build. After editing the UI,
  `:4201` won't change until you rebuild the bundle. Use `:8080` for live work, or
  rebuild + hard-refresh (Ctrl+Shift+R) `:4201`.
- **PowerShell + UTF-8 scripts**: PS 5.1 reads BOM-less UTF-8 as Windows-1252, which
  mangles non-ASCII (em-dashes, etc.) and can break parsing. Keep the `scripts/*.ps1`
  **ASCII-only** (this is why the dashes here are `--`).
- **SSE event named `error`**: the browser `EventSource` reserves `error` for its own
  connection-failure event, so the daemon's failure event is named **`failed`** — don't
  rename it back.
