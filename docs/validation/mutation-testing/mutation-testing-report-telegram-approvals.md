# telegram-approvals — Mutation Testing Report

**Date:** 2026-08-24
**Status:** historical
**Authority:** evidence
**Scope:** `liberado-telegram-approvals`, full lib.

## Campaign history

| Ledger row | Survived | Caught | Unviable | Timeout |
|---|---:|---:|---:|---:|
| `82b28558` | 13 | 56 | 15 | 1 |
| `8622244` (fresh baseline) | 13 | 56 | 15 | 1 |
| `ab712bb` (final) | **1** | 68 | 15 | 1 |

## What was killed

- **Decision recording (`record_decision` + both match arms)** — a bot with an
  attached `ApprovalLedger` records Approved and Rejected taps under their
  proposal id; every dropped-recording mutant leaves `decision_for` empty.
- **Archived-proposal ack (`ack_read_failure`)** — with the active note gone and
  `archive/approved/<stem>.md` present, the bot answers "Already approved."
  instead of "Proposal not found."
- **Sequence numbering (`handle_message`)** — first message is seq 1. A `-`
  underflows on message one; a `*` makes the first reply read as stale.
- **Concurrency notices (`run_chat_turn` gate ×3)** — zero in-flight turns get
  no note; one gets the singular ("is still running"), several the plural.
- **Stale labelling (`!=` → `==`)** — answering the newest seq carries no
  marker; answering an older seq opens with `↩ re: "<text>"`. Both directions
  asserted.
- **Chat-turn delivery (`run_chat_turn` body → `()`)** — the scripted reply
  reaches `send_text`.
- **Revision write-failure guard** — with the proposals tree made read-only,
  the flow stops before any "Revised — please review" announcement.

## Accepted survivor

| Location | Mutant | Why it stands |
|---|---|---|
| `lib.rs:169` | `log_startup_banner` → `()` | Pure `tracing::info!`; nothing observable depends on the banner text. |

## Harness notes

- **turbovault handles index lazily per handle.** A file written through vault
  handle A is invisible to handle B until B touches that path itself. Seed
  fixtures through the same handle the code under test reads from — the
  inline archived-proposal test passes only because its write happens to ride
  on the handle the assertion later reads. Symptom when bitten: `Err(ENOENT)`
  from the bot's vault while the original handle reads the file fine.
- `signer.sign()` returns `SignedProposal` (Deref to `Proposal`); use
  `.into_proposal()` when a plain `Proposal` value is needed.
- `run_chat_turn` decrements `in_flight`, so direct calls must pre-increment or
  they underflow; pass the chat surface as `Arc<dyn ChatSurface>` explicitly.
