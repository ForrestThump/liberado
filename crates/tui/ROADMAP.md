# liberado-tui — Roadmap

**Canonical maturity plan (2026-07-10):**  
[`docs/future-work/tui-maturity-roadmap.md`](../../docs/future-work/tui-maturity-roadmap.md)

That document is the living audit vs Claude Code / Grok Build / OpenCode / KiloCode / VTCode and the
phased plan (T0–T8): performance, **goal-session mode**, freeze/verifiers UI, coding density, palette,
multi-session reliability.

---

## Historical note

Sections below recorded **engineering hardening** completed mid-2026 (mutexes, SSE timeout, mouse,
themes, etc.). They are **not** a claim of product parity with agentic coding TUIs. Prefer the
maturity roadmap for all new work.

---

## Completed engineering backlog (archive)

See git history and the prior revision of this file for full writeups of:

- Esc / Ctrl+S stream cancel, markdown + themes, conversation search, status bar  
- Production hardening (parking_lot, SSE timeout, message cap, SIGTERM, effect tests)  
- Mouse hit-testing, bounded channels, SSE parse errors  

## Deferred library split

`DECOMPOSITION.md` (`agent-tui-core`) remains a **post-maturity** option (phase T7). Do not start
until goal mode + performance phases land.
