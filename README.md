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

Configuration uses a **mesh / layered precedence** (later wins):

1. Built-in `Default`
2. `LIBERADO_CONFIG_DIR/<file>` (optional env override)
3. repo-root `config/<file>` (runtime overrides, git-ignored)
4. `<crate>/config/<file>` (compile-time examples only)

Three optional TOML files (all may be omitted):

| File           | Purpose                     |
|----------------|-----------------------------|
| `topology.toml` | vault path, MCPs, providers |
| `policy.toml`   | security zones & grants     |
| `tuning.toml`   | behavior knobs              |

Starter examples live in `config.example/`.

### Minimal setup (no env vars)
Drop a `config/topology.toml` next to the crate with:
```
vault_path = "C:\\path\\to\\vault"
```
Without `DEEPSEEK_API_KEY` the daemon runs in **watch-only** mode.

### With environment variables
```cmd
set DEEPSEEK_API_KEY=sk-...
set LIBERADO_VAULT=C:\path\to\vault
```

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
