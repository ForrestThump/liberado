# Code metrics

Committed quality-gate baselines and configs live here (kept out of the repo root):

| Artifact | Role |
|----------|------|
| `cargo-crap.toml` | Documented twin of root `.cargo-crap.toml` (cargo-crap discovers only the root filename). `threshold = 29.9` is the new-function CRAP limit. |
| `function-complexity.toml` | New-function cyclomatic ceiling (`new_function_ceiling = 20`). No per-function baseline and no waiver. |
| `module-health.toml` + `*-baseline.json` | Per-file module health |
| `unwrap-classification.toml` + `*-baseline.json` | Unwrap / expect classifier ratchet |
| `mutants-ledger.json` | Append-only cargo-mutants campaign ledger |

CRAP and cyclomatic scores for a pull request live in gitignored `.liberado/diff-gate-report.json`. A merge-base CRAP report, when the gate needs one, is `.liberado/crap-base-<sha>.json`.

Scratch under `.liberado/` is unchanged and stays gitignored.
