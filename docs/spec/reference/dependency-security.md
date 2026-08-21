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
   review. Do not add a name-only exception.
4. Run `just dependency-security` and request the CODEOWNER review.

GitHub Actions and the two sibling path dependencies use full commit SHAs.
Dependabot proposes Action SHA changes. Update sibling SHAs in
`.github/actions/checkout-siblings/action.yml` only after reviewing their
diffs. Checkout credentials are never persisted into a compiling job.

The cargo-vet exemption set is the accepted graph at the time this gate was
introduced. It is debt, not an audit claim. New exemptions may not be added
without a written review reason, and the set should shrink as audits become
available.
