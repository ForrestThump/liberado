# Liberado — Quick Start (Windows 10)

## Prerequisites
- Rust 1.90+ via rustup: https://rustup.rs
- (Optional) `DEEPSEEK_API_KEY` for dispatcher/chat

## Build
```cmd
cargo build --release
```

Binary: `target\release\liberado.exe`

## Configuration

**Config directory** (Windows): `%APPDATA%\liberado\`
(source: `dirs::config_dir()/liberado` in `crates/bootstrap/src/config.rs:68`)

Three optional TOML files (all may be omitted):

| File           | Purpose                     |
|----------------|-----------------------------|
| `topology.toml` | vault path, MCPs, providers |
| `policy.toml`   | security zones & grants     |
| `tuning.toml`   | behavior knobs              |

Starter examples live in `config.example/`.

### Without environment variables
- Put `vault_path = "C:\\path\\to\\vault"` in `topology.toml`
- No `DEEPSEEK_API_KEY` → daemon runs in **watch-only** mode

### With environment variables
```cmd
set DEEPSEEK_API_KEY=sk-...
set LIBERADO_VAULT=C:\path\to\vault
```

`LIBERADO_CONFIG_DIR` may override the config directory.

## Run the background daemon
```cmd
cargo run --release --bin liberado -- serve
```
(or `liberado.exe serve` after `cargo install --path crates/cli`)

Listens on `http://0.0.0.0:4201`

## Run the TUI chat client
Open a second terminal:

```cmd
cargo run --release --bin liberado -- chat
```

Type `exit` or Ctrl-D to quit.

Resume a session:

```cmd
cargo run --release --bin liberado -- chat <session-id>
```
