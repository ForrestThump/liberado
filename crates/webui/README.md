# liberado-webui

The Dioxus (WASM) web UI for Liberado. Pure frontend: the browser talks to the **daemon's HTTP API on port 4201**, no embedded agent logic.

This README is the iteration handoff for the UI: dev loop, layout, styling system, still-true gotchas. Wire contract: [`docs/spec/reference/api.md`](../../docs/spec/reference/api.md). Crate role and dependencies: [`docs/spec/reference/crate-map.md`](../../docs/spec/reference/crate-map.md). Why the slim build excludes this crate: [`docs/spec/reference/slim-build.md`](../../docs/spec/reference/slim-build.md).

---

## Working on the WebUI

Pre-reqs: rustup toolchain with `wasm32-unknown-unknown` installed (see [`#build-commands`](#build-commands)) and the Dioxus CLI (`dx`). Copy `config.example/ops.toml` to `.liberado/ops.toml` first.

```bash
just dev-start --vault <path-to-vault>      # detached daemon on :4201
just webui-dev                              # Dioxus hot reload on :8080
just stop-webui-dev                         # stop :8080
just stop-daemon                            # stop :4201
```

The dev server on `:8080` calls the daemon's API on `:4201` (see [`api_base`](#api_base--cors)), so the daemon must also be running. `:4201` serves a *static release bundle* of this crate — it does not change until you rebuild (next section). Hard-refresh after a rebuild, or just use `:8080`.

### Build commands

The slim build excludes this crate (WASM-only); see [`docs/spec/reference/slim-build.md`](../../docs/spec/reference/slim-build.md). `dx` must use the rustup `cargo` — the standalone Rust on `PATH` lacks the wasm32 std. Verify with `rustup target list --installed` showing `wasm32-unknown-unknown`.

```bash
# Release bundle the daemon's ServeDir serves (DIST_DIR in crates/server/src/lib.rs).
# target/dx/liberado-webui/release/web/public
dx build -r -p liberado-webui --web

# Hot-reload dev server on :8080 (the justfile recipe wraps this).
dx serve -p liberado-webui --platform web --addr 0.0.0.0 --port 8080

# Host-build tests (browser half has no DOM runner; CI exercises it via dx build).
cargo test -p liberado-webui
```

CI builds the same bundle in the `webui` job (`.github/workflows/ci.yml`) and asserts `manifest.json`, `sw.js`, `index.html` are present and well-formed.

### Ship to the homelab

```bash
just deploy-webui-homelab                # build the bundle and ship it
just deploy-webui-homelab --skip-build
```

The bundle is **mounted** into the daemon's container; `ServeDir` re-reads it per request — no image rebuild, no restart. GitHub Actions also **bakes** a release bundle into `ghcr.io/forrestthump/liberado` (`BAKE_WEBUI=1`); `deploy/homelab/setup.sh` serves that baked copy. **Do not** use `just deploy-homelab` for a frontend-only change — that rebuilds the daemon image and takes 20–40 minutes. See `deploy/homelab/README.md`.

---

## Source layout

`crates/webui/`:

- **Shell**: `index.html` (PWA), `public/{manifest.json, sw.js, icon-*.png}`. `sw.js` shows a Web Push and focuses the app on click. It does not cache and it does not intercept `/api/`.
- **App**: `src/main.rs` (App + `Route` match + `api_base`), `src/shell.rs` (`GrokAppHeader`, `PhoneMenuFab`, viewport helpers), `src/back_nav.rs` (phone Back: history entries mirror open layers), `src/pwa.rs` (PWA-shell assertions), `src/theme.rs` (`theme_css_vars(&Theme)` → `:root { --lib-* }`), `src/icons.rs`.
- **Styles**: `src/styles/main.css` — all styling, 279 semantic classes.
- **Components** (`src/components/`): chat pipeline (`chat.rs`, `chat_input.rs` with the pure `stream_url()` builder, `chat_fold.rs`, `chat_reasoning.rs`, `chat_scroll.rs` + tests, `chat_submission.rs`, `chat_tool_events.rs`, `message_row.rs`, `markdown.rs` — `pulldown-cmark` with trailing-WS trim, `conversation_row.rs`); slash commands (`slash_commands.rs`, `slash_palette.rs`); routes (one component per Route: `library.rs`, `automations.rs`, `projects.rs`, `plugins.rs`, `imagine.rs` — Route::Imagine reserved); sidebar submodule (`sidebar/mod.rs` — `Route` enum + `Sidebar` — `sidebar/{rail,agents,projects,profile_api,row_menu,tests}.rs`, `sidebar_agent_creator.rs`, `sidebar_shelf.rs`); header overlays (`incognito.rs`, `grants_popover.rs` Tools, `model_browser.rs`, `profile_browser.rs`, `profile_surface.rs`, `picker.rs`); helpers (`approval_card.rs`, `mcp_panel.rs`, `project_data.rs`).
- **Dashboard (orphan)**: `dashboard.rs` plus its panels (`reactions.rs`, `vault.rs`, `profiles_panel.rs`, `stuck_sessions.rs`) still compile, but no `Route` targets `dashboard.rs` — reachable only through the module tree (`pub mod dashboard` in `components/mod.rs`) until the Status view returns.

---

## Routes and pages

`components/sidebar/mod.rs::Route` is the canonical enum. `is_chat_view()` returns true for `Chat | ChatSession | AgentSession | ProjectDetail`; all four render through the single `Chat` component.

| Route | Renders | Notes |
|---|---|---|
| `Chat`, `ChatSession`, `AgentSession`, `ProjectDetail` | `Chat` | Fresh / existing chat / agent conversation / coding project. |
| `Library` | `components::library::Library` | |
| `Automations` | `components::automations::Automations` | |
| `Projects` | `components::projects::Projects` | |
| `Plugins` | `components::plugins::Plugins` | |
| `Agents` | `components::imagine::Imagine` | Reuses `Imagine`; sidebar section header pushes `route.set(Route::Agents)`. |
| `Imagine` | `components::imagine::Imagine` | **Reserved** — kept in the enum and on disk so the surface can be re-introduced without re-shaping the rail; the sidebar never renders a primary-nav row targeting it. |

The sidebar's **primary nav** renders only `Chat`, `Library`, `Automations` (`PrimaryNavRow` calls in `sidebar/mod.rs`). Other routes are reached via section headers ("Projects", "Plugins") or the Agents shelf. The **collapsed rail** renders Chat, Search, Library, Automations, Agents and Plugins plus the theme swatch — `Tools` is **not** in the rail, it lives in the header (`shell::GrokAppHeader`'s tools popover, opened through `tools_open` in `main.rs`). `shell::shell_route_title(route)` sets the header bar title; chat views stay "Liberado", every other route uses its view name.

---

## Data flow

- `main.rs::api_base()` computes the daemon base URL once and threads it to every view. **Same-origin by default** (`window.location.origin`) — whoever served the page also answers `/api/*`. The one exception is `dx serve`, which runs on `:8080` and cannot proxy; there, and only there, retarget the same host's `:4201`. See [`api_base`](#api_base--cors).
- Wire DTOs come from the **`chat-client-contract`** crate — single source of truth shared with the TUI and CLI.
- Chat streams over **SSE**:
  - `GET /api/chat/stream?message=…` — one turn. `session=<id>`, `incognito=true`, and `profile=<name>` are mutually exclusive **optional** selectors (at most one ever appears on the URL — the builder's match arms short-circuit in that order): `session` names an existing conversation, `incognito=true` opens a RAM-only one, `profile=<name>` opens a new one under that named profile. With **none** of the three (the default first-turn flow), the URL is just `?message=…` and the server creates a fresh normal conversation (`chat_stream_core` → `ChatCreation::Default` → `sessions.create(None)`). `model=<name>` is orthogonal to all three — and to the no-selector case — and appends in every branch. URL builder in `components::chat_input::stream_url` (`chat_input.rs` lines 61-93).
  - `GET /api/conversations/{id}/attach` — join a turn already in flight.
  - `POST /api/conversations/{id}/cancel` — stop the turn.
- Coding projects: `GET /api/projects` — both the sidebar's projects shelf and the projects page fetch through `project_data::fetch_projects`.
- Status / capabilities: `GET /api/status` — slash commands (`/theme`, `/model`, …) and the chat header.
- SSE event types (`chat_client_contract::SessionEvent::from_sse_data`): bare — `session` (id), `token` (text); JSON — `session_started`, `role_started`, `role_finished`, `tool_started`, `tool_finished`, `progress`, `awaiting_input`, `human_input`, `validation_finished`, `critic_verdict`, `file_changed`, `checkpoint`, `loop_guard`, `session_finished`, `session_offered`, `failed`. **Unknown** types return a benign empty `Token`. `done` is **not** an SSE event type — it is the `status` value inside a `session_finished` payload.

Sidebar endpoints:

- `/api/conversations`, `/api/conversations/search`, and `/api/conversations/{id}` rename + delete — `components/sidebar/mod.rs`.
- `/api/profiles/{name}` — `components/sidebar/profile_api.rs`.
- `/api/projects` — fetched from the projects shelf in `components/sidebar/projects.rs` (via `project_data::fetch_projects`).

The full surface is in [`docs/spec/reference/api.md`](../../docs/spec/reference/api.md).

---

## Styling system

No Tailwind, no CSS-in-JS, no build step for styles.

1. **Theme tokens → CSS custom properties.** `theme.rs::theme_css_vars(&Theme)` produces a `:root { --lib-accent: …; --lib-app-bg: …; … }` block from a flat `liberado_theme::Theme` struct. `main.rs` injects the block **before** `main.css`:

   ```rust
   style { {crate::theme::theme_css_vars(&crate::theme::theme_by_name(&theme_name()))} }
   style { {include_str!("./styles/main.css")} }
   ```

   `theme_by_name` reads the saved name from localStorage (`theme::saved_theme_name`, written by `/theme set` and the sidebar's swatch). Two structural surfaces are *derived* from existing tokens (not new theme fields): `--lib-surface` (panel / assistant-bubble bg) and `--lib-surface-2` (code / tool-chip bg).

2. **Semantic classes in `main.css`** that reference `var(--lib-*)`. Components only set class names; no inline colors. To restyle, edit `main.css`; to recolor, edit the theme token (or add one and map it in `theme.rs`).

**CSS authoring constraint.** The stylesheet is `include_str!`-injected as the text content of a Dioxus `<style>` element, and Dioxus text-escapes `<` and `&`. **Avoid `<` and `&`** in `main.css`. `>` (child combinator), `+` (sibling), and descendant selectors are fine — `main.css` already uses `.grok-main-phone-scroll > .grok-page` and similar.

**Class inventory** (279 top-level classes, all live in the current set) groups as: shell (`.app`, `.app-header*`, `.grok-header*`, `.grok-main*`, `.phone-menu-fab`, `.sidebar-menu-btn`, `.header-right-cluster`); sidebar (`.sidebar*`: rail, brand, shelves, sections, scroll, search, empty, error, retry); chat (`.messages`, `.bubble-row`, `.bubble`, `.bubble-wrap`, `.bubble-thinking`, `.input-bar`, `.input`, `.send-btn`, `.markdown-body`); route pages (`.library-page`, `.automations-page`, `.projects-page`, `.plugins-page`, `.imagine-page`, `.automation-list`, `.automation-row`, `.plugin-list`, `.project-list`); cross-cutting (`.grok-page*`, `.grok-row*`, `.incognito-btn`, `.theme-swatch-*`, `.tools-popover`, `.approval-card`, `.slash-palette`, `.mcp-server`, `.conv-item`, `.conv-menu`). Reuse before inventing new ones; list with `rg -oN '^\.[a-zA-Z][a-zA-Z0-9_-]*' crates/webui/src/styles/main.css | sort -u`.

---

## api_base & CORS

`main.rs::api_base()` (wasm build) is **same-origin by default** — it returns `window.location.origin`, because whoever served the page also answers `/api/*`:

| Served by | Page origin | API base |
|---|---|---|
| the daemon, directly | `http://<host>:4201` | same |
| homelab nginx / Traefik → the daemon | `https://liberado.homelab.local` | same |
| `dx serve` (dev) | `http://<host>:8080` | `http://<host>:4201` |

`dx serve` is the one exception: it cannot proxy, so port `8080` — and only `8080` — retargets the daemon. That cross-port case is what `CorsLayer::permissive()` on the daemon exists for; the two same-origin cases need no CORS at all. **Don't reintroduce a hardcoded `:4201`**: at `https://liberado.homelab.local/` the WASM would ask for `https://liberado.homelab.local:4201`, where Traefik does not listen. The base must also stay **absolute** — `reqwest`'s wasm client runs it through `Url::parse`, which rejects a relative path.

---

## PWA

The crate-root `index.html` plus `public/manifest.json`, `public/sw.js`, and the 192/512 PNG icons are the install shell. `dx build` copies `public/` to the served directory root, so the service worker is `/sw.js` and its scope is the origin. Do not move the worker under `/assets/` — a worker at `/assets/sw.js` only controls `/assets/`, and Chrome will not treat the site as an app.

The worker does not cache and does not intercept `/api/` — chat streams over EventSource against `/api/chat/stream` and `/api/conversations/{id}/attach`. HTTPS is required on a phone; `http://<lan-ip>:4201` will not show an install prompt. `pwa.rs` asserts the whole shape compiles; the iOS home-screen meta tags and `viewport-fit=cover` / `interactive-widget=resizes-content` are in `index.html`.

---

## Gotchas (still true)

- **Wasm std / `dx`.** Building for wasm fails with *"can't find crate for `core`"* if `dx` picks up the standalone Rust on `PATH` (it lacks the wasm32 target). Always prepend the rustup `cargo` bin dir; verify with `rustup target list --installed` showing `wasm32-unknown-unknown`.
- **Stale `:4201`.** The daemon serves a *static* release build. After editing the UI, `:4201` does not change until you rebuild the bundle. Use `:8080` for live work, or rebuild + hard-refresh `:4201`.
- **PowerShell + UTF-8 scripts.** PS 5.1 reads BOM-less UTF-8 as Windows-1252, mangling non-ASCII (em-dashes, etc.) and breaking parsing. There are **none today** (no `scripts/*.ps1` exists); a future one must stay ASCII-only (the dashes here are `--`).
- **SSE event named `error`.** Browser `EventSource` reserves `error` for its own connection-failure event, so the daemon's failure event is named **`failed`** — do not rename it back. Asserted in `crates/chat-client-contract/src/wire.rs::failed_tag_is_failed_not_error`.
