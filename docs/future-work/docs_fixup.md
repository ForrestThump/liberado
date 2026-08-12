---
kind: plan
status: active
authority: implementation
domain: docs
canonical_for: docs-lifecycle
open_items: true
---

I’ll audit the documentation as an information system: volume and status distribution, entry points, duplicate subjects, archives, generated references, and where decisions or validation evidence currently live. Then I’ll propose a retention and navigation model that improves agent retrieval without losing useful history.

---

The repository has high information value but only a medium signal-to-noise ratio. The main problem is not weak writing or poor folder structure. It is unclear authority and incomplete document lifecycle management.

My rough assessment:

| Area | Signal-to-noise | Assessment |
|---|---:|---|
| Cold-start navigation | 7/10 | `AGENTS.md`, the docs index, roadmap, and architecture overview give agents a good entry path. |
| Code-adjacent behavior | 7/10 | All 40 top-level library crates have crate Rustdoc, often with strong rationale. |
| Active planning | 3/10 | Too many files can look active, authoritative, or current at the same time. |
| Decisions | 5/10 | Important reasoning exists, but it is concentrated in a large, mutable decision document. |
| Validation evidence | 7/10 | Good mutation and test evidence is retained, but historical evidence is not always clearly separated from current guarantees. |
| Overall | about 5/10 | High signal, but too much stale authority around it. |

The repository currently has 168 Markdown files and about 27,800 lines. Of those, 105 are under `future-work`, 47 are already archived, and 38 remain at the `future-work` root. Nine of those root files have no structured status. The [future-work index](README.md) still calls landed work “active,” including two completed parallel-deliverable rounds.

That is the central issue: the repository retains useful information well, but does not consistently tell an agent whether a document is an instruction, evidence, a proposal, or history.

## Should implemented plans be archived?

Yes, but only after a small distillation step.

Code and tests can document:

- what exists;
- how an API behaves;
- what invariant is enforced now;
- what fails if the invariant is broken.

They usually do not preserve:

- why this design won;
- which alternatives were rejected;
- what failed in production;
- which model or harness behavior motivated the change;
- what a mutation campaign proved at a given commit;
- which apparently sensible approaches did not work.

Therefore, the completion rule for a plan should be:

1. Move current behavior and invariants into Rustdoc, tests, or a normative specification.
2. Move the durable design choice into an ADR.
3. Move measurements and mutation results into a dated evidence record.
4. Remove completed items from the active backlog.
5. Archive the full implementation plan if it still contains useful investigation history.
6. Delete it from the working tree if it contains only implementation steps already expressed better elsewhere. Git still retains it.

A completed plan should never remain in the active index only because it contains one useful paragraph. Extract that paragraph first.

Partially implemented plans should have a short “Open work” section at the top. Completed slices can move to an evidence or history section, or to a separate archived report.

## Establish one authority model

I would define this table as repository policy:

| Question | Authority |
|---|---|
| What does the program do now? | Code, tests, and crate Rustdoc |
| What must remain true across crates? | `docs/spec/reference` and `docs/spec/architecture/contracts.md` |
| Why was this design selected? | Accepted ADR |
| What should an agent implement next? | `backlog.md` only |
| What is the broader direction? | `roadmap.md` |
| What did an experiment prove? | Dated validation/evidence record |
| What might we do later? | Proposed plan or idea |
| What happened before? | Archive and git history |

This resolves an existing ambiguity. The [docs index](../README.md) calls the decision log “frozen,” while the [decision log](../spec/architecture-decisions.md) describes itself as living and mixes completed decisions, open questions, and recommendations. The project questions file even still asks which source should win.

## Use machine-readable document metadata

Folder names are not enough. Each managed document should have a small header such as:

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
  - old-harness-roadmap.md
open_items: true
---
```

Use a small fixed vocabulary:

- `status`: `draft`, `active`, `implemented`, `superseded`, `historical`
- `kind`: `architecture`, `reference`, `decision`, `plan`, `finding`, `validation`, `runbook`
- `authority`: `normative`, `implementation`, `advisory`, `evidence`

Then generate the indexes instead of maintaining them manually.

CI should reject:

- a root `future-work` document without metadata;
- two active documents with the same `canonical_for`;
- an `implemented` or `superseded` plan in the active index;
- an active plan with no open items;
- a normative document that points to an archived document as its authority;
- a generated index that differs from the committed copy.

This would improve agent performance more than another folder move.

## Use more Rustdoc, but enforce its quality

Yes. Rustdoc is the best home for current crate behavior because it stays close to the code. This repository already has a strong base: all 40 top-level `lib.rs` files have `//!` crate documentation, and many public items are well documented.

However, more Rustdoc without checks will create another stale layer. I found 72 source comments that still refer to old paths such as `docs/architecture/...` and `docs/roadmap/...`. One example is the header of `crates/coder-agent/src/lib.rs`. The Markdown link checker does not scan those raw Rust source references.

I would:

- Make crate Rustdoc authoritative for role, public behavior, invariants, errors, and extension seams.
- Keep `ARCHITECTURE.md` only when a crate needs a cross-module diagram, dependency rules, or operational explanation that does not fit Rustdoc.
- Remove “not done yet” lists from crate architecture documents. They belong in the backlog.
- Enable `rustdoc::broken_intra_doc_links`.
- Add `cargo doc --workspace --no-deps` to CI.
- Add `missing_docs` incrementally, starting with foundation and public library crates.
- Add a small linter for obsolete repository-doc paths inside `.rs` files.
- Generate configuration, CLI, crate inventory, and possibly API reference from typed source where practical.

Do not duplicate the same module map, defaults table, or public contract in Rustdoc and Markdown.

## Replace the large decision log with real ADRs

The current decision log has 19 decisions in about 484 lines and was last updated on 2026-07-02. It mixes:

- the question before the decision;
- the recommendation;
- the accepted result;
- later clarifications;
- superseding language.

That structure makes retrieval difficult and encourages conflicts.

Move toward:

```text
docs/decisions/
  README.md
  ADR-0001-daemon-first.md
  ADR-0002-capability-narrowing.md
  ADR-0003-session-as-unit-of-work.md
```

Each ADR should contain:

- status: proposed, accepted, or superseded;
- decision date;
- context;
- decision;
- consequences;
- rejected alternatives;
- implementation and test links;
- supersedes/superseded-by links.

ADRs should be mostly immutable. A later change creates a new ADR that supersedes the old one. A generated index can preserve the existing decision numbers.

## Preserve evidence as evidence

The [validation index](../validation/README.md) is already a good pattern: one summary document with supporting per-crate reports.

Strengthen each result record with:

- exact commit;
- date;
- command;
- operating system and important environment facts;
- tool and model version;
- mutation that was applied;
- artifact location or digest;
- conclusion;
- whether the conclusion is still current.

Tests are current executable evidence. A mutation report is historical evidence that the test caught a defect at a particular revision. Both are useful, but they answer different questions.

Raw traces, large logs, and repeated run artifacts should live in an artifact store or dated result bundles. Markdown should contain the conclusion, provenance, and durable links.

## Make it wiki-like without adding another source of truth

I would not introduce an independently edited wiki. It would drift from the repository.

Instead, generate a wiki-like site from the repository Markdown and Rustdoc:

- full-text search;
- generated status/domain indexes;
- backlinks;
- “supersedes” and “validated by” relationships;
- links from system documents to crate Rustdoc;
- links from ADRs to plans, tests, and evidence;
- a generated `SUMMARY.md` for mdBook or a similar static renderer.

The important feature is the document graph, not the visual skin.

## Highest-value sequence

I would improve the system in this order:

1. Define the authority table and metadata schema.
2. Add metadata and linting to the 38 root `future-work` documents.
3. Archive obvious completed and superseded plans after distillation.
4. Generate `future-work/README.md` and a repository-wide document catalog.
5. Split the architecture decision log into ADRs.
6. Repair the 72 stale source-document references.
7. Add Rustdoc build and link checks to CI.
8. Reduce crate `ARCHITECTURE.md` files to material that Rustdoc cannot express.
9. Add the generated searchable documentation site.

The goal should not be fewer documents at all costs. It should be a much smaller active surface, with rich history still available behind explicit evidence, decision, and archive boundaries.
