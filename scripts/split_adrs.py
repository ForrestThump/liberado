#!/usr/bin/env python3
"""Split docs/spec/architecture-decisions.md into individual ADR files.

Preserves decision numbers as ADR-NNNN. Writes docs/decisions/ and a generated
README index. Leaves a stub pointer at the old path.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "docs" / "spec" / "architecture-decisions.md"
OUT_DIR = ROOT / "docs" / "decisions"

# Slugs for decisions 1–19 (stable identifiers)
SLUGS = {
    1: "invocation-model-and-inference",
    2: "daemon-first-process-model",
    3: "mcp-transport-and-process-model",
    4: "capability-zone-model",
    5: "vault-concurrency-and-provenance",
    6: "event-delivery-and-idempotency",
    7: "monorepo-workspace",
    8: "subagent-execution-model",
    9: "hook-messages-via-vault",
    10: "secrets-and-inter-component-auth",
    11: "proposal-and-approval-boundary",
    12: "runtime-audit-tracing",
    13: "provider-capability-floor",
    14: "single-source-config",
    15: "frontmatter-schema-validation",
    16: "testing-seams-for-dispatch",
    17: "conversation-history-store",
    18: "incremental-event-bus-mesh",
    19: "turbovault-as-privileged-plugin",
}


def split_decisions(text: str) -> list[tuple[int, str, str]]:
    """Return list of (number, title, body) from the monolithic log."""
    # Match ### N. Title
    pattern = re.compile(r"^### (\d+)\.\s+(.+)$", re.MULTILINE)
    matches = list(pattern.finditer(text))
    results: list[tuple[int, str, str]] = []
    for i, m in enumerate(matches):
        num = int(m.group(1))
        title = m.group(2).strip()
        start = m.end()
        end = matches[i + 1].start() if i + 1 < len(matches) else len(text)
        body = text[start:end].strip()
        results.append((num, title, body))
    return results


def extract_status(body: str) -> str:
    if re.search(r"\*\*Status\*\*:\s*Complete", body, re.I):
        return "accepted"
    if re.search(r"Status:\s*Complete", body, re.I):
        return "accepted"
    if re.search(r"\*\*Status\*\*:\s*superseded", body, re.I):
        return "superseded"
    return "accepted"


def build_adr(num: int, title: str, body: str) -> str:
    slug = SLUGS.get(num, f"decision-{num}")
    status = extract_status(body)
    # Prefer a Decision N: block as the decision summary when present
    decision_match = re.search(
        r"(?:Decision\s+%d[:\s]+)(.+?)(?:\n\n|\n\*\*|\nTrade-offs|\nKey |\Z)" % num,
        body,
        re.DOTALL | re.I,
    )
    decision_text = (
        decision_match.group(1).strip() if decision_match else "See body for full decision text."
    )
    # Truncate very long decision pull-quote
    if len(decision_text) > 1200:
        decision_text = decision_text[:1200].rstrip() + "…"

    context = ""
    why = re.search(r"\*\*Why it matters\*\*:\s*(.+?)(?:\n\n|\n\*\*)", body, re.DOTALL)
    if why:
        context = why.group(1).strip()
    else:
        context = f"Recorded as Decision {num} in the historical architecture decision log."

    impl_links: list[str] = []
    for link in re.findall(r"`([^`]+\.md)`|\[([^\]]+)\]\(([^)]+\.md)\)", body):
        target = link[0] or link[2]
        if target:
            impl_links.append(target)
    impl_links = list(dict.fromkeys(impl_links))[:12]

    lines = [
        "---",
        "kind: decision",
        f"status: {status}",
        "authority: normative",
        "domain: architecture",
        f"canonical_for: adr-{num:04d}",
        "open_items: false",
        "---",
        "",
        f"# ADR-{num:04d}: {title}",
        "",
        f"**Status:** {status}  ",
        "**Date:** 2026-07-02 (last update of the consolidated decision log; see git history for earlier revisions)  ",
        f"**ID:** ADR-{num:04d} (`{slug}`)",
        "",
        "## Context",
        "",
        context,
        "",
        "## Decision",
        "",
        decision_text,
        "",
        "## Consequences",
        "",
        "See the full decision body below for implications, trade-offs, and interactions with other ADRs.",
        "",
        "## Rejected alternatives",
        "",
        "Where the original log listed open options and a recommended path, the recommended path is the accepted decision. Alternatives discussed in the body were not adopted as the primary design.",
        "",
        "## Implementation and tests",
        "",
    ]
    if impl_links:
        for t in impl_links:
            lines.append(f"- `{t}`")
    else:
        lines.append("- See crate Rustdoc and tests for the current implementation of this decision.")
    lines.extend(
        [
            "",
            "## Supersedes / superseded by",
            "",
            "- **Supersedes:** (none — original decision number from the consolidated log)",
            "- **Superseded by:** (none)",
            "",
            "## Full historical body",
            "",
            "The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.",
            "",
            "---",
            "",
            body,
            "",
        ]
    )
    return "\n".join(lines)


def write_index(adrs: list[tuple[int, str, str]]) -> str:
    lines = [
        "---",
        "kind: index",
        "status: active",
        "authority: advisory",
        "generated: true",
        "domain: architecture",
        "---",
        "",
        "# Architecture Decision Records",
        "",
        "Individual ADRs replace the former monolithic `docs/spec/architecture-decisions.md`.",
        "ADRs are **mostly immutable**. A later change adds a new ADR that supersedes an old one.",
        "",
        "Authority: accepted ADRs answer *why this design was selected*. Current behavior lives in",
        "code, tests, and Rustdoc. Cross-crate contracts live in",
        "[`docs/spec/architecture/contracts.md`](../spec/architecture/contracts.md).",
        "",
        "| ADR | Title | Status | File |",
        "|-----|-------|--------|------|",
    ]
    for num, title, body in adrs:
        slug = SLUGS.get(num, f"decision-{num}")
        fname = f"ADR-{num:04d}-{slug}.md"
        status = extract_status(body)
        lines.append(f"| ADR-{num:04d} | {title} | {status} | [{fname}]({fname}) |")
    lines.extend(
        [
            "",
            "## Writing a new ADR",
            "",
            "1. Allocate the next number.",
            "2. Create `ADR-NNNN-short-slug.md` with sections: status, date, context, decision,",
            "   consequences, rejected alternatives, implementation/test links, supersedes/superseded-by.",
            "3. If replacing an older decision, set **superseded-by** on the old ADR and **supersedes** on the new one.",
            "4. Re-run this index generation if automated; otherwise update this table.",
            "",
        ]
    )
    return "\n".join(lines)


def main() -> int:
    if not SOURCE.is_file():
        print(f"missing source: {SOURCE}", file=sys.stderr)
        return 1
    raw = SOURCE.read_bytes()
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        text = raw.decode("cp1252")
    adrs = split_decisions(text)
    if len(adrs) < 19:
        print(f"expected >= 19 decisions, found {len(adrs)}", file=sys.stderr)
        return 1
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    for num, title, body in adrs:
        slug = SLUGS.get(num, f"decision-{num}")
        path = OUT_DIR / f"ADR-{num:04d}-{slug}.md"
        path.write_text(build_adr(num, title, body), encoding="utf-8", newline="\n")
        print(f"wrote {path.relative_to(ROOT).as_posix()}")
    index = write_index(adrs)
    (OUT_DIR / "README.md").write_text(index, encoding="utf-8", newline="\n")
    print("wrote docs/decisions/README.md")

    stub = """---
kind: index
status: superseded
authority: advisory
domain: architecture
superseded_by: ../decisions/README.md
---

# Architecture decisions (moved)

This monolithic log is **superseded** by individual ADRs:

**→ [`docs/decisions/README.md`](../decisions/README.md)**

Decision numbers ADR-0001 through ADR-0019 are preserved. Do not edit this file for new decisions.
"""
    SOURCE.write_text(stub, encoding="utf-8", newline="\n")
    print("replaced docs/spec/architecture-decisions.md with stub")
    return 0


if __name__ == "__main__":
    sys.exit(main())
