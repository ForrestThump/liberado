# Paseo ↔ Liberado integration

**Status:** Multi-mode ACP bridge is live (`liberado-acp`). Paseo registers **one**
`extends: "acp"` provider; Liberado owns coding / chat / face splitting via ACP
`session/set_mode` (and process default `--mode` / `LIBERADO_ACP_MODE`).

## What you get

- Paseo detects **Liberado** as an agent (provider list / diagnostics).
- Starting a session in Paseo runs `liberado-acp`, which speaks ACP JSON-RPC 2.0 and
  exposes four **modes** on the same process:

| Mode | Engine | Needs daemon? |
|---|---|---|
| **coding** (default) | Interactive conversation + coding tools on a durable worktree | No |
| **goal** | One-shot coding pack ([`LiberadoLoopBackend`](../../crates/coder-agent/)) to a terminal | No |
| **chat** | In-process `Conversation` + executor (no file tools) | No |
| **face** | HTTP SSE to `liberado serve` (`POST /api/chat/stream`) — vault tools + `delegate` | Yes (`liberado serve`) |

- Switch modes mid-session with ACP `session/set_mode` (Paseo mode picker when present),
  or set the process default:
  - `liberado-acp --mode chat`
  - `LIBERADO_ACP_MODE=face`
- Coding mode: lasting conversation with the same coding tools and durable worktree
  (`coding-worktrees/<session>` when `cwd` is a git repo). One prompt is one turn. It does
  **not** run the pack's outer attempt/repair/ship-bar loop. Switch to **goal** for that.
- Goal mode: the previous default — one prompt is one `CoderRunRequest` through
  [`LiberadoLoopBackend`](../../crates/coder-agent/), with ship preflight before a success
  claim (PR #134). Same `[coder]` tuning. HTTP `POST /api/goals` remains the GUI/agent
  one-shot API on the daemon.

> **Set `LIBERADO_CONFIG_DIR` in the Paseo provider entry.** The bridge resolves through
> `liberado_config::config_dir()`, but `liberado-acp` is installed to `~/.cargo/bin`, so the
> walk-up-from-the-binary tier finds no repo `config/`, and the platform config dir usually holds no
> `topology.toml`. Without an explicit value the agent runs on defaults: **no declared project, so no
> ship bar, and an empty capability grant.** That was the state for the whole dogfood period and
> nothing reported it. `scripts/install-paseo-liberado.ps1` now writes the variable; check the
> `config directory resolved` line the bridge logs at startup to confirm which directory it picked
> and which of the three files it found.
>
> **Keep `provider = "openrouter"` at the top level of `topology.toml`.** TOML table scope is
> positional. If the key appears after `[main_agent]`, it is `main_agent.provider`, not the global
> provider selection. Before the config guard was added, that unknown field was ignored, the global
> provider stayed at its `deepseek` default, and Paseo diagnostics reported only three models even
> though the file appeared to select OpenRouter. Current builds reject the misplaced key during
> config load. `liberado config check` catches it without starting Paseo.
- Face mode is the **only** path that tunnels into a running daemon. Coding, goal, and chat are
  self-contained agent processes (same pattern as Claude Code / Gemini / Grok on Paseo).

### Residual

| Feature | Status |
|---|---|
| Live hub `/goal` list in Paseo UI | Separate (daemon HTTP bridge) |
| Token-by-token tool events mid-coding-run | Follow-up (pack currently reports at end) |
| Intake clarify questions via ACP | Interactive coding offers `ask_human`; the next `session/prompt` is the answer. Goal-mode intake still uses the pack's `InputChannel` (not ACP). |
| Face-mode cancel mid-stream | Cooperative cancel wired (drops SSE future); daemon may still finish its turn |

## Prerequisites (Windows)

1. **Rust toolchain** matching `rust-toolchain.toml` (workspace builds `liberado-acp`).
2. **Sibling checkouts** for path deps (same as the rest of Liberado):

   ```powershell
   # from life-os/
   git clone <fork>/turbovault turbovault; git -C turbovault checkout develop
   git clone <fork>/turbomcp turbomcp;   git -C turbomcp  checkout develop
   ```

3. An LLM API key in the environment (any one). **Prefer OpenRouter** so the model
   picker gets `author/model` ids (`deepseek/deepseek-v4-pro`, …) from live `GET /models`:

   | Env var | Base URL | Default model | Picker catalog |
   |---|---|---|---|
   | `OPENROUTER_API_KEY` (**preferred**) | `https://openrouter.ai/api/v1` | `deepseek/deepseek-v4-pro` | Full live OpenRouter catalog (`author/model`), A–Z |
   | `DEEPSEEK_API_KEY` | `https://api.deepseek.com/v1` | `deepseek-chat` | Live DeepSeek `/models` |
   | `OPENAI_API_KEY` | `https://api.openai.com/v1` | `gpt-4o-mini` | Live OpenAI `/models` |

   Or set `LIBERADO_CONFIG_DIR` to a Liberado config directory with a working
   `[[topology.providers]]` entry (same resolution as the daemon).

4. Optional: `LIBERADO_ACP_MODEL` to override the initial model id (e.g.
   `deepseek/deepseek-v4-flash`). Paseo's model picker calls ACP `session/set_model`
   to hot-swap; the catalog is built from the backend's live `/models` endpoint.

5. Optional: `LIBERADO_ACP_MAX_TURNS` — turns **per user message** (default **50**).
   Coding maps this to `CoderRoleConfig::max_turns`; chat uses it as the executor budget.
   Raise it for large refactors; lower it for cheap probes.

6. Optional: `LIBERADO_ACP_MODE` / `liberado-acp --mode coding|goal|chat|face` — process default mode
   for new sessions (ACP `session/set_mode` can still switch later).

7. Face mode only: `liberado serve <vault>` running, and optional `LIBERADO_SERVER`
   (default `http://127.0.0.1:4201`).

## Install `liberado-acp` on PATH

From the Liberado repo root:

```powershell
cargo install --path crates/acp-bridge --force
# binary name: liberado-acp.exe  →  %USERPROFILE%\.cargo\bin\
```

Confirm:

```powershell
where.exe liberado-acp
# smoke (no model call):
'{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientInfo":{"name":"t","version":"0"}}}' `
  | liberado-acp
```

You should see a JSON-RPC result with `"protocolVersion":1` and `"agentInfo":{"name":"Liberado",…}`.

Helper script (writes Paseo config as well):

```powershell
powershell -File scripts/install-paseo-liberado.ps1
```

## Register Liberado in Paseo

Paseo home defaults to `%USERPROFILE%\.paseo` (or `$env:PASEO_HOME`).

Edit (or create) `%USERPROFILE%\.paseo\config.json`:

```json
{
  "agents": {
    "providers": {
      "liberado": {
        "extends": "acp",
        "label": "Liberado",
        "description": "Liberado multi-mode agent (coding · chat · face)",
        "command": ["liberado-acp"],
        "env": {
          "LIBERADO_ACP_MODEL": "deepseek/deepseek-v4-pro"
        },
        "params": {
          "supportsMcpServers": false
        }
      }
    }
  }
}
```

**One provider is enough.** Do not register three Paseo providers with different launch args —
modes are Liberado-owned (`session/set_mode` / `--mode` / `LIBERADO_ACP_MODE`).

Notes:

- `supportsMcpServers: false` — Liberado already owns tools in coding/face; skip Paseo’s injected
  MCP catalog (some ACP adapters refuse non-empty `mcpServers` on `session/new`).
- Prefer the bare command `liberado-acp` once it is on `PATH`. Absolute path also works:
  `["C:\\Users\\You\\.cargo\\bin\\liberado-acp.exe"]`.
- **API keys live in the environment that starts Paseo** (`OPENROUTER_API_KEY`, etc.), not in
  `config.json`. Do not paste secrets into the provider `env` block.
- Optional default mode via env: `"LIBERADO_ACP_MODE": "chat"` (still switchable in-session).

Example file in-repo: [`config.example/paseo-liberado.json`](../../config.example/paseo-liberado.json).

## Build / run Paseo (from the checkout under this repo)

```powershell
cd paseo
pnpm install
pnpm build   # or the package scripts in paseo/README.md
# then launch desktop or CLI per Paseo docs
```

After install, open Paseo’s provider list — **Liberado** should appear. Run diagnostics on it:
you want green rows for launcher binary, ACP `initialize`, and ACP `session/new`.

## Protocol contract (what we speak)

| Direction | Method | Purpose |
|---|---|---|
| Client → agent | `initialize` | Negotiate `protocolVersion: 1` |
| Client → agent | `session/new` | `{ cwd, mcpServers }` → `{ sessionId, models, modes }` |
| Client → agent | `session/prompt` | `{ sessionId, prompt: ContentBlock[] }` → `{ stopReason }` |
| Client → agent | `session/set_mode` | `{ sessionId, modeId: coding\|goal\|chat\|face }` |
| Client → agent | `session/set_model` | Hot-swap model id from catalog |
| Agent → client | `session/update` | `agent_message_chunk`, `tool_call`, `tool_call_update` |
| Client → agent | `session/cancel` | Notification; chat turns return `stopReason: "cancelled"` |
| Client → agent | `session/load` | Not advertised (`loadSession: false`) until durable history |

## Limits (honest)

- Session history is **process-local** today. Durable resume across restarts is future work.
- **Coding / chat** are self-contained. **Face** requires a running `liberado serve` and
  streams its events into ACP; cancel mid-face-stream is not wired yet.
- Coding pack live tool streaming mid-run is a follow-up (report streams at end today).

**Ordered residual work** (tool-call ids, resume honesty, diagnostics, modes, durable load, fork
polish, remote track):
[`future-work/paseo-liberado-integration-roadmap.md`](../future-work/paseo-liberado-integration-roadmap.md).

## Troubleshooting

| Symptom | Check |
|---|---|
| Provider missing in Paseo | `config.json` under `PASEO_HOME` / `~/.paseo`; restart Paseo |
| Diagnostics: launcher not found | `where liberado-acp`; re-run `cargo install --path crates/acp-bridge` |
| Diagnostics: initialize timeout | Binary hung — ensure nothing else is reading stdin; run the smoke pipe above |
| Prompt errors about API key | Export `DEEPSEEK_API_KEY` (or peer) in the env that starts Paseo / the agent |
| Tools write to wrong tree | Session `cwd` comes from Paseo’s workspace; open the project you intend |
| Face mode: cannot reach daemon | Start `liberado serve <vault>`; check `LIBERADO_SERVER` |
| Want pure chat without pack | Paseo mode picker → **chat**, or `LIBERADO_ACP_MODE=chat` / `--mode chat` |
