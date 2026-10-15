# Dependency security admission

**Status**: current

CI treats dependency compilation as arbitrary code execution. The
`dependency-security` job resolves the locked graph and runs cargo-deny and
cargo-vet before any job may compile a build script or procedural macro.

## Updating dependencies

1. Update `Cargo.toml` and `Cargo.lock` together.
2. Run `cargo vet`. A new crate or version needs a trusted audit, a delta
   audit, or an explicit reviewed exemption in `supply-chain/config.toml`.
3. If the graph gains a build script, add its exact `name@version` package
   specification to `[bans.build].allow-build-scripts` in `deny.toml` after
   review. Do not add a name-only exception. A patched crate can require
   companion version bumps (for example `rustls` pulling `aws-lc-rs`,
   `aws-lc-sys`, and `rustls-webpki`). Update the lockfile, the allow-list
   entries, and the cargo-vet exemptions together.
4. Run `just dependency-security` and request the CODEOWNER review.

`deny.toml` allows the build script `rustls@0.23.46`. The vet exemption
in `supply-chain/config.toml` names the same version. 0.23.46 is the
2026-10-14 security release. It replaces 0.23.45, which cargo-deny
rejects for the unauthenticated ServerHello key-exchange downgrade.
The build script is the same file as in 0.23.45. The exemption is
still debt, not an audit.

GitHub Actions use full commit SHAs. Dependabot proposes Action SHA changes.
`turbovault*` and `turbomcp*` are git+tag pins on the public ForrestThump forks.
`turbovault*` is pinned to `tag = "liberado-2026-10-15"` and `turbomcp*` stays
on `tag = "liberado-2026-08-27"`. `Cargo.lock` records the resolved commit SHA.
Update tags, lockfile entries, and matching cargo-vet exemptions together after
reviewing the fork diff. Checkout credentials are never persisted into a
compiling job.

The cargo-vet exemption set is the accepted graph at the time this gate was
introduced. It is debt, not an audit claim. New exemptions may not be added
without a written review reason, and the set should shrink as audits become
available.

## Yanked crates

`deny.toml` sets `yanked = "warn"` rather than `"deny"`. A transitive crate
can be yanked upstream at any moment after it enters the lockfile, and when
no newer version exists (as happened to `bisync 0.3.0` under
`gix-protocol`) there is no code fix — only a policy change unblocks CI.
Yanked warnings are still surfaced so the drift stays visible.

## Unmaintained advisories

`deny.toml` `[advisories].ignore` lists advisories that mark a crate unmaintained
when there is no known vulnerability and no safe upgrade. Each entry names the
crate and the upstream that should drop it.

- `RUSTSEC-2024-0436` (`paste`) is transitive through fastembed/tokenizers and ratatui.
- `RUSTSEC-2026-0192` (`ttf-parser`) is transitive through eframe's Linux window stack.

## YAML

Proposal notes, memory notes, and docs frontmatter use `serde_norway` 0.9.
`serde_yaml` 0.9 is deprecated and depends on unmaintained `unsafe-libyaml`
(`RUSTSEC-2024-0449`). `serde_yml` is unsound and unmaintained
(`RUSTSEC-2025-0068`), so cargo-deny rejects it. `serde_norway` is the
maintained hard fork of the `serde_yaml` 0.9 API. It depends on
`unsafe-libyaml-norway` instead of `unsafe-libyaml`, so that advisory is not
in the graph. The callers are `liberado-common`, `liberado-cli`, and
`liberado-memory-store`.
