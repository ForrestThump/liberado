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

This policy is the current authority. The source audit that led to it is retained
as historical analysis in the future-work archive.

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
| `status` (plans, findings, indexes, policy, validation, architecture, reference, runbook) | `draft`, `active`, `implemented`, `superseded`, `historical` |
| `status` (kind `decision` / ADRs) | `draft`, `proposed`, `accepted`, `superseded`, `historical` |
| `kind` | `architecture`, `reference`, `decision`, `plan`, `finding`, `validation`, `runbook`, `index`, `policy` |
| `authority` | `normative`, `implementation`, `advisory`, `evidence` |

Other fields (`domain`, `canonical_for`, `supersedes`, `superseded_by`,
`open_items`, `last_verified`, `verified_against`, `generated`) are optional
except where the lint rules require them.

**Managed documents** are every `docs/**/*.md` file that carries YAML frontmatter,
plus every root `docs/future-work/*.md` plan (which must have frontmatter). Vocabulary
and required fields are enforced on **all** managed documents, not only root plans.

## CI rejection rules

`just docs-meta-check` rejects:

1. A root `docs/future-work/*.md` document without metadata.
2. A managed document missing `kind` / `status` / `authority`, or using a value outside the kind-aware vocabulary (e.g. `status: banana` on an ADR).
3. Two **active** (or decision `accepted`/`proposed`) documents with the same `canonical_for`.
4. An `implemented` or `superseded` plan listed in the active future-work index.
5. An **active** plan with `open_items` not set to `true`.
6. A **normative** document that links into `archive/` as content authority.
7. A generated index (`docs/future-work/README.md`, `docs/CATALOG.md`) that differs from the generator output.

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
