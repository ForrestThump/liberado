---
kind: reference
status: active
authority: normative
domain: ci
canonical_for: local-readiness
open_items: false
last_verified: 2026-08-21
---

# Local readiness

`just ready` is the fast pre-push gate on Windows and Debian. It checks formatting, locked
metadata, workspace Clippy, architecture and workflow rules, module health, host-stable
per-function complexity, and documentation contracts. Success writes `.liberado/ready.json`.

The receipt binds the current commit, tracked changes, and untracked files. A commit, amend,
merge, rebase, conflict resolution, or content change makes it stale. `just verify-ready`, the
committed pre-push hook, and `just push` reject a stale receipt. Enable the hook with:

```console check=false
just setup-hooks
```

Coverage-sensitive CRAP remains a Debian authority. Run `just crap-linux` after Rust control-flow
changes. It runs natively on Debian/Linux. On Windows it maps the checkout into the Debian WSL
distribution and runs the same Rust CLI command there.
The default distribution name is `Debian`; set `LIBERADO_DEBIAN_WSL_DISTRO` when the installed
Debian-compatible distribution uses another name.

The host-stable function ratchet is configured in `function-complexity.toml` and committed in
`function-complexity-baseline.json`. Existing functions may not gain cyclomatic complexity. New
functions must stay under the configured ceiling. A persistent exception must name one exact file
and function and include an explicit ceiling, reason, and review date.
