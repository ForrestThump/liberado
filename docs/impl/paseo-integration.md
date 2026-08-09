# Paseo ↔ Liberado integration

**Status:** ACP bridge is live (`liberado-acp`). Paseo registers it as a generic
`extends: "acp"` provider and spawns it over stdio.

## What you get

- Paseo detects **Liberado** as a coding agent (provider list / diagnostics).
- Starting a session in Paseo runs `liberado-acp`, which:
  - Speaks real ACP JSON-RPC 2.0 (`initialize`, `session/new`, `session/prompt`, …).
  - Roots coding tools at the session `cwd` (`read_file`, `write_file`, `run_command`, …).
  - Streams assistant text and tool activity as `session/update` notifications.

This is **not** a tunnel into a running `liberado serve` daemon. It is the Liberado
coding stack packaged as an ACP agent process — the same pattern Gemini CLI / Hermes
use with Paseo.

## Prerequisites (Windows)

1. **Rust toolchain** matching `rust-toolchain.toml` (workspace builds `liberado-acp`).
2. **Sibling checkouts** for path deps (same as the rest of Liberado):

   ```powershell
   # from life-os/
   git clone <fork>/turbovault turbovault; git -C turbovault checkout develop
   git clone <fork>/turbomcp turbomcp;   git -C turbomcp  checkout develop
   ```

3. An LLM API key in the environment (any one):

   | Env var | Base URL used |
   |---|---|
   | `DEEPSEEK_API_KEY` | `https://api.deepseek.com/v1` |
   | `OPENROUTER_API_KEY` | `https://openrouter.ai/api/v1` |
   | `OPENAI_API_KEY` | `https://api.openai.com/v1` |

   Or set `LIBERADO_CONFIG_DIR` to a Liberado config directory with a working
   `[[topology.providers]]` entry (same resolution as the daemon).

4. Optional: `LIBERADO_ACP_MODEL` to override the model slug (default `deepseek-chat`).

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
        "description": "Liberado coding agent (ACP)",
        "command": ["liberado-acp"],
        "env": {
          "LIBERADO_ACP_MODEL": "deepseek-chat"
        },
        "params": {
          "supportsMcpServers": false
        }
      }
    }
  }
}
```

Notes:

- `supportsMcpServers: false` — Liberado already owns tools; skip Paseo’s injected MCP catalog
  (some ACP adapters refuse non-empty `mcpServers` on `session/new`).
- Prefer the bare command `liberado-acp` once it is on `PATH`. Absolute path also works:
  `["C:\\Users\\You\\.cargo\\bin\\liberado-acp.exe"]`.
- **API keys live in the environment that starts Paseo** (`DEEPSEEK_API_KEY`, etc.), not in
  `config.json`. Do not paste secrets into the provider `env` block.

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
| Agent → client | `session/update` | `agent_message_chunk`, `tool_call`, `tool_call_update` |
| Client → agent | `session/cancel` | Notification; turn returns `stopReason: "cancelled"` |
| Client → agent | `session/load` | Re-open session id in this process |

## Limits (honest)

- Session history is **process-local** today (`session/load` reopens a fresh conversation with the
  same id). Durable resume across restarts is future work.
- Not a remote tunnel into `liberado serve`. For remote Liberado, see the roadmap “Remote access
  via Paseo” item separately.
- Full face-agent dispatch / vault MCP grants are not mounted in this bridge; the session is the
  coding-tool surface (`CodingToolRuntime`) plus the configured chat model.

## Troubleshooting

| Symptom | Check |
|---|---|
| Provider missing in Paseo | `config.json` under `PASEO_HOME` / `~/.paseo`; restart Paseo |
| Diagnostics: launcher not found | `where liberado-acp`; re-run `cargo install --path crates/acp-bridge` |
| Diagnostics: initialize timeout | Binary hung — ensure nothing else is reading stdin; run the smoke pipe above |
| Prompt errors about API key | Export `DEEPSEEK_API_KEY` (or peer) in the env that starts Paseo / the agent |
| Tools write to wrong tree | Session `cwd` comes from Paseo’s workspace; open the project you intend |
