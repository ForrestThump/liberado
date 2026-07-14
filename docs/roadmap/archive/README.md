# Archive — finished plans, closed audits, resolved findings

Nothing in here describes the system as it is now. These are **completed plans**, **closed audits**,
and **resolved findings** — kept because the *reasoning* is often worth more than the outcome (why a
thing was built this way, what was tried and rejected, what a bug actually turned out to be), and
deleting them would make the live docs look like they arrived by magic.

They were moved here on 2026-07-14 because 32 roadmap files, 21 of them dead, made the live ones
unfindable. A roadmap you cannot navigate is not a roadmap.

**If you are looking for what is true today, none of this is it.** Start with:

- [`../current.md`](../current.md) — what is live, what is next, what is known-broken.
- [`../../architecture/overview.md`](../../architecture/overview.md) — the cold-start map.
- [`../../architecture/failure-modes.md`](../../architecture/failure-modes.md) — **the distilled
  lessons from the audits in this directory.** Read this one. It is the reason it is safe to archive
  the rest: the individual audits found the same handful of bugs over and over, and that pattern —
  not the incident detail — is the part worth carrying forward.

Statuses inside these files were true when written and may be wrong now (`session-focus-plan.md`
still says "no code yet"; S1–S7 all shipped). They are a record, not a claim.

Everything here is in git history regardless; the archive just keeps it reachable without noise.
