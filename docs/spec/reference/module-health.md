# Module health ratchet

**Status**: current

`just module-health` uses `rust-code-analysis-cli` 0.0.25 to measure production
Rust files under `crates/*/src/`. It tracks physical lines, logical lines,
function and closure count, and aggregate cyclomatic complexity separately.
Install the pinned tool with:

```text
cargo install rust-code-analysis-cli --version 0.0.25 --locked
```

The committed `module-health-baseline.json` is the last accepted state. A file
that is above a review boundary cannot get worse. A new file must stay below
the hard new-file ceilings in `module-health.toml`. This is a review signal,
not proof that a file has a bad design.

Use a `[[waiver]]` only after review shows that a large file is cohesive, such
as a declarative protocol table. A waiver names one file, one metric, a reason,
a review date, and a hard ceiling. It does not disable the other metrics.
Stale, duplicate, and unnecessary waivers fail the check.

Not every waiver request is accepted. Before proposing one: move test modules
into `#[path]` sibling files first (test growth is never waiver material); name
what the file *is* and why splitting it would hide something; and put a real
ceiling and review date on it. Reasons that read as laziness — "tests are
long", "splitting is churn", "it grew" — get the contribution pushed back for
rework. The acceptance bar lives as comments beside the `[[waiver]]` example in
`module-health.toml`.

The mutant-hardening campaign recorded a narrow class of accepted waivers that
follow this rule exactly: the parent module's growth is only the irreducible
three- or four-line `#[cfg(test)] #[path]` sibling wiring that declares the
split test file, while the survivor tests themselves live in the sibling.
Those waivers carry a hard `ploc` ceiling and a review date; any further growth
in the parent module is not covered and must be split, not waived.

When a later split honestly drops that sibling below every review boundary,
delete the waiver. Do not keep the old ceiling as slack, and do not raise a
ceiling to absorb a merge.

`crates/main-agent/src/sessions/tests.rs` had four metric waivers (ploc 3640,
lloc 1053, functions 175, cyclomatic 226). Sharing session fixtures and
splitting tests by state made those ceilings unnecessary; they are gone from
`module-health.toml`. The committed `module-health-baseline.json` is still the
last accepted measurement — this change does not raise it.

The same narrow rule applies when a production crate root crosses its boundary
only because it declares and exports a new sibling module. The waiver must name
the exact wiring lines and exclude implementation growth. `crates/cost/src/lib.rs`
uses this form for the split `latency_report.rs` module and its compatibility
fixture field; the report implementation stays outside the waived root.

After an accepted improvement, run `just module-health-ratchet` and commit the
lower baseline. GitHub runs only the read-only comparison.

## License

The analyzer is an unmodified external build and CI tool. Liberado does not
link or copy its source. `rust-code-analysis` is provided by Mozilla under
MPL-2.0. Its source and license are available at
<https://github.com/mozilla/rust-code-analysis>. Keep that notice and the exact
version pin when the tool is upgraded.
