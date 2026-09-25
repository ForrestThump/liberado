# Module health ratchet

**Status**: current

`just module-health` uses `rust-code-analysis-cli` 0.0.25 to measure production
Rust files under `crates/*/src/`. It tracks physical lines, logical lines,
function and closure count, and aggregate cyclomatic complexity separately.
Install the pinned tool with:

```text
cargo install rust-code-analysis-cli --version 0.0.25 --locked
```

The committed `code-metrics/module-health-baseline.json` is the last accepted state. A file
that is above a review boundary cannot get worse. A new file must stay below
the hard new-file ceilings in `code-metrics/module-health.toml`. This is a review signal,
not proof that a file has a bad design.

The check reuses a current generated JSON report when one is available and runs the analyzer when
it is not. An unreadable or invalid report is an error; it never becomes an empty healthy report.
After a successful check, the ratchet adds passing new files and saves lower metrics. For each
existing file, it keeps the old value when current PLOC, LLOC, function count, or cyclomatic
complexity is higher. Growth below a review boundary can pass, but it cannot raise the baseline.

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
`code-metrics/module-health.toml`.

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
`code-metrics/module-health.toml`. The committed `code-metrics/module-health-baseline.json` is still the
last accepted measurement — this change does not raise it.

CAS1 (chat vs. agent surface mode) wires two narrow waivers for the irreducible
introductions on `crates/main-agent/src/sessions.rs` (the new `AgentProfiles`
field + the `#[path = "sessions/agent_profiles.rs"] mod agent_profiles;`
declaration) and on `crates/session-store/src/jsonl.rs` (the +1 cyclomatic
and +1 closure for the new chat-lens projection closure). The builder itself
lives in the sibling modules so this file's structured-functionality floor
does not regress; the waivers carry hard ceilings and review dates so any
further growth is not covered and must be split, not waived.

The same narrow rule applies when a production crate root crosses its boundary
only because it declares and exports a new sibling module. The waiver must name
the exact wiring lines and exclude implementation growth. `crates/cost/src/lib.rs`
uses this form for the split `latency_report.rs` module, its compatibility
fixture field, and the `#[cfg(test)] #[path]` sibling wiring that declares
`survivor_tests.rs`; the report and survivor-test implementations stay outside
the waived root.

Similarly, god-file test suites (`crates/daemon/src/tests.rs` and `crates/coder-agent/src/lib.rs`)
are partitioned into modular sibling files (`crates/daemon/src/tests/*.rs`, `lib_unit_tests.rs`,
`lib_loop_tests.rs`, `lib_disposition_tests.rs`) to eliminate monolithic test blocks and preserve
clean module health without synthetic waivers.

The cross-provider fallback feature wires three narrow waivers for irreducible growth on the
config-schema side. `crates/config-loader/src/model/topology.rs` grows by one struct (`ProviderFallback`),
one default helper, and one cross-cutting doc comment — a schema addition that has no other home.
`crates/config-loader/src/model/config.rs` adds two error checks inside `validate_providers`'s
per-provider loop (self-reference guard + undeclared-name guard). The functions metric grew by
one closure, `providers.iter().any(|p| p.name == fb.provider)`; that waiver's ceiling is the
measured count, 88. Splitting the checks into a helper adds another function and its own
cyclomatic, so the checks stay inline.
`crates/config-loader/src/model/builder.rs` adds a shared `provider_profile` fixture at module scope so
the sibling `builder_fallback_tests.rs` can use it via `super::provider_profile` — the
fallback-specific tests themselves live in the sibling and add zero regression to `builder.rs`.
The three waivers carry hard ceilings and review dates; any further growth in these files must be
split, not waived.

The mechanical-jobs feature (ADR-0020) wires narrow waivers for the wiring-side growth that the
schedule-schema additions push onto the dispatch composition. `crates/bootstrap/src/lib.rs` carries
a `cyclomatic` (135), `functions` (80), and `ploc` (1100) waiver: the new `attach_reminder`
function (the optional-notifier `match`) and `startup_snapshots` (the `filter` + `map` pipeline
that turns enabled `git-snapshot` schedules into `JobRequest`s) are wired into
`wire_dispatch_stack`, where the dispatch stack itself is composed — moving either helper to a
sibling module would split the wiring from the dispatch composition that owns it.
`crates/notify/src/lib.rs` carries a `cyclomatic` (150) and `functions` (90) waiver: the new
`TelegramNotifier::from_reminder_env` is a thin loader parallel to the existing `from_env` (one
`let-else` per env var, two total), and the two loaders are kept side-by-side so the wiring in
`bootstrap` can pick the right one by env name. The three config-loader `ploc` ceilings from the
cross-provider fallback feature (`builder.rs` 1130 → 1200, `config.rs` 1080 → 1120,
`topology.rs` 1120 → 1150) absorb the `CronSchedule` field plumbing this PR added on the schedule
schema (nine new fields with their doc comments) — every one of them belongs on the schema, and
the `config.rs` `functions` ceiling (88 → 90) absorbs the module-scope `validate_schedule_job`
helper that was split out to keep `validate_schedules` at its cyclomatic baseline. All five
carry hard ceilings and review dates; any further growth in these files must be split, not waived.

After an accepted improvement, run `just module-health-ratchet` and commit the
lower baseline. The command does not save worse values. Linux `just ci` also
ratchets this file; other hosts compare only. GitHub runs only the read-only
comparison.

## License

The analyzer is an unmodified external build and CI tool. Liberado does not
link or copy its source. `rust-code-analysis` is provided by Mozilla under
MPL-2.0. Its source and license are available at
<https://github.com/mozilla/rust-code-analysis>. Keep that notice and the exact
version pin when the tool is upgraded.
