# Liberado — One-Minute Quickstart (Windows)

## 1. Clone & build
```cmd
git clone https://github.com/your-org/life-os
cd life-os
cargo build --release
```

Binary lands at `target\release\liberado.exe`.

## 2. Minimal config (one line)
Create `config/topology.toml` (git-ignored):
```toml
vault_path = "C:\\Users\\You\\YourVault"
```
That’s it — no other env vars needed for watch-only mode.

## 3. Start the daemon + web UI
```cmd
cargo run --release --bin liberado -- serve
```
- Listens on `http://0.0.0.0:4201`
- Open `http://localhost:4201` in any browser for the web dashboard.
- Optional env for chat + dispatcher:
  ```cmd
  set DEEPSEEK_API_KEY=sk-...
  ```

## 4. Chat via TUI (second terminal)
```cmd
cargo run --release --bin liberado -- chat
```
Type `exit` or Ctrl-D to quit. Resume with `chat <session-id>`.

## Config precedence (if you need more)
Later wins: built-in defaults → `LIBERADO_CONFIG_DIR` → repo `config/` → crate `config/`.
All three files (`topology.toml`, `policy.toml`, `tuning.toml`) are optional—absent files simply keep defaults. Starter files live in `config.example/`.
