# liberado-coder-tools - coding tool runtime

`liberado-coder-tools` exposes the small, boring tools the Rust-native coding loop will offer to the
model through `liberado-executor::ToolRuntime`.

## Contract

- The runtime returns structured JSON strings to the model for successful calls.
- Tool failures are returned as `Err(String)` so the executor feeds them back in-band and the model
  can adapt.
- Path containment comes from `liberado-coder-sandbox`; this crate does not concatenate arbitrary
  model paths.
- Prompts and model choices do not live here.

## Initial Tool Catalog

- `list_files`
- `search_text`
- `read_file`
- `write_file`
- `edit_file`
- `git_status`
- `git_diff`
- `run_command`
- `validate`

The catalog is intentionally decomposed. Higher-level behavior belongs in `coder-agent`, which can
choose which tools a role sees and can layer progress guards over the event stream.

## Next Steps

- Add deterministic `apply_patch` once the patch schema is settled.
- Route command execution through Docker sandbox implementations.
- Emit `CoderEvent` values for every invocation.
- Add output shaping tuned by `heuristics-tuner` scenarios instead of changing tool semantics.
