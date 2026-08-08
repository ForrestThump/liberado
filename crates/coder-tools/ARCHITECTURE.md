# liberado-coder-tools - coding tool runtime

`liberado-coder-tools` exposes the small, boring tools the Rust-native coding loop will offer to the
model through `liberado-executor::ToolRuntime`.

## Contract

- The runtime returns structured JSON strings to the model for successful calls.
- Tool failures are returned as `Err(String)` so the executor feeds them back in-band and the model
  can adapt.
- Path containment comes from `liberado-coder-sandbox`; this crate does not concatenate arbitrary
  model paths.
- Runtime construction can select host-local or Docker workspace execution from `SandboxSpec`.
- Prompts and model choices do not live here.

## Initial Tool Catalog

- `list_files`
- `search_text`
- `read_file`
- `write_file`
- `edit_file`
- `apply_patch`
- `hashline_edit` (when `[coder.hashline] enabled = true`)
- `git_status`
- `git_diff`
- `run_command`
- `validate`

The catalog is intentionally decomposed. Higher-level behavior belongs in `coder-agent`, which can
choose which tools a role sees and can layer progress guards over the event stream.

`apply_patch` is currently a conservative atomic multi-edit tool: each edit names a file plus one
exact `old`/`new` replacement. The runtime validates path policy, file existence, non-empty old text,
and exactly-one match for every edit before it writes any file. This gives models a compact
multi-file edit affordance without introducing a broad textual patch parser as an authority boundary.

`hashline_edit` (optional, default off) is a line-anchored patch dialect ported from oh-my-pi:
`read_file` emits `[path#TAG]` content-hash headers and `LINE:content` rows; patches use
`PUT`/`CUT`/`REM` against those original line numbers and fail closed on a stale tag.
Configure via `[coder.hashline]` in `tuning.toml` (`enabled`, `hash_length` 4–10; alphabet `0-9A-Z`).

### Tests

| Layer | Where | What |
|---|---|---|
| Pure engine | `hashline.rs` unit tests | Hash alphabet/length, parse/apply ops, REM, multi-section atomic preflight, stale/missing tags |
| Tool runtime | `lib.rs` integration tests | Catalog gating, partial-read full-file tags, stale reject without write, multi-file, path policy, cut/insert |
| Config | `coder-core` | `HashlineConfig` bounds, serde defaults, `[coder.hashline]` tuning parse |
| Agent wiring | `coder-agent` mock tests | Prompt appendix + catalog when enabled/disabled; mock `hashline_edit` end-to-end |
| Live | `openrouter_deepseek_live_hashline_edit_smoke` (ignored) | Real model against OpenRouter |

```text
cargo test -p liberado-coder-tools --lib hashline
cargo test -p liberado-coder-core --lib hashline
cargo test -p liberado-coder-agent --lib hashline
cargo test -p liberado-coder-agent openrouter_deepseek_live_hashline_edit_smoke -- --ignored
```

## Next Steps

- Wire live Docker smoke coverage once the backend can run an end-to-end task in a container.
- Emit `CoderEvent` values for every invocation.
- Add output shaping tuned by `heuristics-tuner` scenarios instead of changing tool semantics.
