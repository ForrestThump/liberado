# Documentation maintenance

Use this skill before a pull request changes behavior, configuration, commands,
protocols, crate boundaries, or operational procedures. Use it also for a
scheduled documentation audit.

## Establish authority

1. Read `docs/spec/reference/doc-authority.md`.
2. Identify the implementation source of truth: code, tests, schema, manifest,
   CLI help, or configuration type.
3. Find the active normative or implementation document. Do not treat archived
   plans or validation records as current instructions.
4. Run `just docs-audit` and `just docs-meta-check` before editing prose.

## Review the changed subsystem

Compare the implementation and documentation for:

- public names and terminology
- defaults and configuration precedence
- accepted inputs and emitted outputs
- failure behavior and recovery steps
- lifecycle and dependency direction
- examples and commands
- security and authorization boundaries

Run safe documented commands. Mark parseable TOML, JSON, and YAML examples with
the `check` fence attribute when they are complete documents rather than
fragments.

## Classify each document

Set or confirm its metadata classification: current, draft, implemented,
superseded, or historical. Update `last_verified` and `verified_against` only
after comparison with the named source. A recent date is not evidence by itself.

Move durable current rules into Rustdoc, tests, or normative specifications.
Keep measurements in validation records. Keep investigation history in archive
documents. Do not rewrite historical evidence as current truth.

## Validate the result

Run:

```text
just check-links
just check-crate-map
just docs-meta-check
just docs-audit
```

If the branch changes a mapped source, run `just docs-impact <base-revision>`.
Use a narrow `[[waiver]]` in `docs-audit.toml` only after human review confirms
that behavior and prose did not change.

In the pull-request description, list:

- implementation authorities checked
- documents updated or confirmed
- commands and examples executed
- remaining uncertainty or unreviewed documents
