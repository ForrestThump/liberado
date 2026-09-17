# Code metrics

Committed quality-gate baselines and configs live here (kept out of the repo root):

| Artifact | Role |
|----------|------|
| `crap-baseline.json` | Per-function CRAP ratchet (Ubuntu host of truth) |
| `cargo-crap.toml` | Documented twin of root `.cargo-crap.toml` (cargo-crap discovers only the root filename) |
| `function-complexity.toml` + `*-baseline.json` | Cyclomatic ratchet |
| `module-health.toml` + `*-baseline.json` | Per-file module health |
| `unwrap-classification.toml` + `*-baseline.json` | Unwrap / expect classifier ratchet |
| `mutants-ledger.json` | Append-only cargo-mutants campaign ledger |

Scratch under `.liberado/` is unchanged and stays gitignored.
