---
kind: policy
status: active
authority: normative
domain: docs
canonical_for: document-authority
open_items: false
last_verified: 2026-08-12
---

# Document authority and metadata

This is repository policy for what each kind of document may claim, and the
machine-readable metadata every managed document must carry.

Source analysis and lifecycle rules: [`docs/future-work/docs_fixup.md`](../../future-work/docs_fixup.md).

## Authority table

| Question | Authority |
|---|---|
| What does the program do now? | Code, tests, and crate Rustdoc |
| What must remain true across crates? | `docs/spec/reference` and `docs/spec/architecture/contracts.md` |
| Why was this design selected? | Accepted ADR under `docs/decisions/` |
| What should an agent implement next? | `docs/future-work/backlog.md` only |
| What is the broader direction? | `docs/roadmap.md` |
| What did an experiment prove? | Dated validation/evidence record under `docs/validation/` |
| What might we do later? | Proposed plan or idea under `docs/future-work/` |
| What happened before? | `docs/future-work/archive/` and git history |

## Fixed metadata vocabulary

Managed documents use YAML frontmatter with at least:

```yaml
---
kind: plan
status: active
authority: implementation
domain: coding-harness
last_verified: 2026-08-12
verified_against: abc1234
canonical_for: harness-comparison
supersedes:
  - old-plan.md
open_items: true
---
```

| Field | Allowed values |
|---|---|
| `status` | `draft`, `active`, `implemented`, `superseded`, `historical` |
| `kind` | `architecture`, `reference`, `decision`, `plan`, `finding`, `validation`, `runbook`, `index`, `policy` |
| `authority` | `normative`, `implementation`, `advisory`, `evidence` |

Other fields (`domain`, `canonical_for`, `supersedes`, `superseded_by`,
`open_items`, `last_verified`, `verified_against`, `generated`) are optional
except where the lint rules require them.

## CI rejection rules

`python scripts/docs_meta.py lint` rejects:

1. A root `docs/future-work/*.md` document without metadata.
2. Two **active** documents with the same `canonical_for`.
3. An `implemented` or `superseded` plan listed in the active future-work index.
4. An **active** plan with `open_items` not set to `true`.
5. A **normative** document that links into `archive/` as content authority.
6. A generated index (`docs/future-work/README.md`, `docs/CATALOG.md`) that differs from the generator output.

## Plan completion rule

When a plan is finished:

1. Move current behavior and invariants into Rustdoc, tests, or a normative specification.
2. Move the durable design choice into an ADR.
3. Move measurements into a dated evidence record under `docs/validation/`.
4. Remove completed items from `backlog.md`.
5. Archive the full plan under `docs/future-work/archive/` if investigation history remains useful.
6. Delete from the working tree only if every useful fact already lives elsewhere (git still retains it).

A completed plan must not stay in the active index only because one paragraph is useful. Extract that paragraph first.

## ADRs

Architecture decisions live as individual files under `docs/decisions/`. Each ADR is mostly immutable. Later change creates a new ADR that supersedes the old one. See [`docs/decisions/README.md`](../../decisions/README.md).

## Evidence provenance

Validation records should record:

- exact commit
- date
- command
- operating system and important environment facts
- tool and model version
- mutation that was applied (if any)
- artifact location or digest
- conclusion
- whether the conclusion is still **current** or **historical**

Tests are current executable evidence. A mutation report is historical evidence that a test caught a defect at a particular revision.
