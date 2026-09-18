# Liberado — first-run quickstart

A few minutes from clone to a running daemon and an open chat. Examples use
bash; on Windows, swap `/` for `\` in paths and use `%APPDATA%\liberado` in
place of `~/.config/liberado`. The `liberado` binary is `target/release/liberado`
on Linux/macOS and `target\release\liberado.exe` on Windows.

## 1. Clone & build

```bash
git clone https://github.com/ForrestThump/liberado
cd liberado
just build-slim                 # day-to-day build (full workspace: just build)
```

For an optimized `liberado` binary (e.g. for a release daemon):

```bash
just build-release              # → target/release/liberado
# or
cargo build --release --bin liberado
```

## 2. Config (optional)

All three files are optional — an absent file keeps the daemon's built-in
defaults. Starters live under `config.example/` (`topology.toml`,
`policy.toml`, `tuning.toml`).

The daemon picks a single directory in this order — first match wins — and
reads all three files from there. This chooses *where the files come from*;
it is separate from how an individual setting's value is resolved once they
are loaded. See
[`docs/spec/reference/tuning.md`](../../spec/reference/tuning.md) §"Where
config lives":

1. `$LIBERADO_CONFIG_DIR`
2. the platform config dir — `~/.config/liberado` (or `%APPDATA%\liberado` on Windows)
3. the directory of the running binary

For value overrides once the directory is chosen — code defaults, then files,
then env (`LIBERADO_*`), then CLI flags; the highest source wins — see
[`docs/spec/config-spec.md`](../../spec/config-spec.md) §"File Layout &
Precedence".

```bash
mkdir -p ~/.config/liberado
cp config.example/topology.toml ~/.config/liberado/topology.toml
```

The minimum useful edit is `vault_path`, the Obsidian vault to watch:

```toml
vault_path = "/path/to/your/vault"
```

`vault_path` is required somewhere — in `topology.toml`, or as the argument to
`liberado serve`. Both empty is a hard error.

## 3. Start the daemon

```bash
target/release/liberado serve /path/to/your/vault
# or, reading the vault from the environment:
LIBERADO_VAULT=/path/to/your/vault target/release/liberado
```

The daemon binds `0.0.0.0:4201` by default (override with `LIBERADO_PORT`).
Open `http://localhost:4201` for the WebUI dashboard. Watch-only mode needs
nothing else.

For chat and any other LLM feature, the daemon also needs a provider API key
in the environment — the name is `topology.toml`'s `[[providers]]` row's
`api_key_env`. Out of the box, the default provider is `deepseek`
(`DEEPSEEK_API_KEY`), and `openrouter` is pre-declared
(`OPENROUTER_API_KEY`):

```bash
export DEEPSEEK_API_KEY=sk-...     # or: export OPENROUTER_API_KEY=sk-or-...
```

## 4. Chat via the CLI (second terminal)

```bash
target/release/liberado chat
# type messages; type 'exit' or press Ctrl-D to quit
```

The client connects to `http://127.0.0.1:4201` by default. Override with
`LIBERADO_SERVER=http://host:port` if the daemon is elsewhere.

Pass a session id to resume a previous conversation:

```bash
target/release/liberado chat 01HZTURNABC…
```

## Where to go next

- [`docs/spec/reference/tuning.md`](../../spec/reference/tuning.md) — every
  config knob, and which ones still need a rebuild
- [`docs/spec/architecture/overview.md`](../../spec/architecture/overview.md) —
  what the daemon actually does
- [`docs/roadmap.md`](../../roadmap.md) — where this is going
