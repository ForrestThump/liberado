#!/usr/bin/env python3
"""Document metadata lint, index generation, and docs lifecycle helpers.

Implements the authority model and machine-readable metadata rules from
docs/future-work/docs_fixup.md (and docs/spec/reference/doc-authority.md).

Subcommands:
  lint          Validate root future-work metadata and generated indexes.
  generate      Write future-work/README.md and docs/CATALOG.md.
  check-stale-rs  Fail if crates/**/*.rs still reference obsolete doc paths.
  self-test     Run pure-logic unit tests (no monorepo required).

Exit code 0 on success; non-zero on lint failures.
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
import textwrap
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

# ---------------------------------------------------------------------------
# Fixed vocabulary (docs_fixup.md)
# ---------------------------------------------------------------------------

STATUS_VOCAB = frozenset(
    {"draft", "active", "implemented", "superseded", "historical"}
)
KIND_VOCAB = frozenset(
    {
        "architecture",
        "reference",
        "decision",
        "plan",
        "finding",
        "validation",
        "runbook",
        "index",
        "policy",
    }
)
AUTHORITY_VOCAB = frozenset(
    {"normative", "implementation", "advisory", "evidence"}
)

REQUIRED_FIELDS = ("kind", "status", "authority")

# Paths under crates that must not appear in .rs sources.
OBSOLETE_RS_PREFIXES = (
    "docs/architecture/",
    "docs/roadmap/",
)

# Remap table for bulk path repair in .rs files.
RS_PATH_REWRITES = (
    (r"docs/architecture/", "docs/spec/architecture/"),
    (r"docs/roadmap/", "docs/future-work/"),
)

FRONTMATTER_RE = re.compile(
    r"\A---\r?\n(.*?)\r?\n---\r?\n?", re.DOTALL
)
CANONICAL_LINK_RE = re.compile(
    r"\]\(((?:\.\./)*(?:future-work/)?archive/[^)#\s]+)\)"
)


# ---------------------------------------------------------------------------
# YAML-lite frontmatter (enough for our fixed schema; no external deps)
# ---------------------------------------------------------------------------


def parse_simple_yaml(block: str) -> dict[str, Any]:
    """Parse a restricted YAML subset used in doc frontmatter."""
    data: dict[str, Any] = {}
    lines = block.splitlines()
    i = 0
    while i < len(lines):
        line = lines[i]
        if not line.strip() or line.strip().startswith("#"):
            i += 1
            continue
        if ":" not in line:
            i += 1
            continue
        key, _, rest = line.partition(":")
        key = key.strip()
        rest = rest.strip()
        if rest in ("", "|", ">"):
            # multi-line list or empty
            items: list[str] = []
            i += 1
            while i < len(lines):
                nxt = lines[i]
                if nxt.startswith("  - ") or nxt.startswith("- "):
                    items.append(nxt.split("-", 1)[1].strip().strip("\"'"))
                    i += 1
                elif nxt.startswith(" ") and ":" not in nxt.strip():
                    i += 1
                else:
                    break
            data[key] = items
            continue
        if rest.lower() in ("true", "false"):
            data[key] = rest.lower() == "true"
        elif (rest.startswith("[") and rest.endswith("]")) or rest.startswith("-"):
            # inline list not used; treat as string
            data[key] = rest.strip("\"'")
        else:
            data[key] = rest.strip("\"'")
        i += 1
    return data


def dump_frontmatter(meta: dict[str, Any]) -> str:
    """Serialize frontmatter with stable key order."""
    order = [
        "kind",
        "status",
        "authority",
        "domain",
        "last_verified",
        "verified_against",
        "canonical_for",
        "supersedes",
        "superseded_by",
        "open_items",
        "generated",
    ]
    lines = ["---"]
    seen: set[str] = set()
    for key in order:
        if key not in meta:
            continue
        seen.add(key)
        val = meta[key]
        if isinstance(val, bool):
            lines.append(f"{key}: {'true' if val else 'false'}")
        elif isinstance(val, list):
            if not val:
                lines.append(f"{key}: []")
            else:
                lines.append(f"{key}:")
                for item in val:
                    lines.append(f"  - {item}")
        else:
            lines.append(f"{key}: {val}")
    for key, val in meta.items():
        if key in seen:
            continue
        if isinstance(val, bool):
            lines.append(f"{key}: {'true' if val else 'false'}")
        elif isinstance(val, list):
            lines.append(f"{key}:")
            for item in val:
                lines.append(f"  - {item}")
        else:
            lines.append(f"{key}: {val}")
    lines.append("---")
    return "\n".join(lines) + "\n"


def split_frontmatter(text: str) -> tuple[dict[str, Any] | None, str]:
    m = FRONTMATTER_RE.match(text)
    if not m:
        return None, text
    return parse_simple_yaml(m.group(1)), text[m.end() :]


def ensure_frontmatter(text: str, meta: dict[str, Any]) -> str:
    existing, body = split_frontmatter(text)
    if existing is None:
        return dump_frontmatter(meta) + "\n" + text.lstrip("\n")
    merged = {**existing, **meta}
    return dump_frontmatter(merged) + body.lstrip("\n") if body.startswith("\n") else dump_frontmatter(merged) + "\n" + body


# ---------------------------------------------------------------------------
# Pure lint logic (testable without monorepo)
# ---------------------------------------------------------------------------


@dataclass
class DocRecord:
    path: str  # posix relative to repo root
    meta: dict[str, Any] | None
    body: str = ""


@dataclass
class LintIssue:
    path: str
    message: str


@dataclass
class LintResult:
    issues: list[LintIssue] = field(default_factory=list)

    @property
    def ok(self) -> bool:
        return not self.issues

    def add(self, path: str, message: str) -> None:
        self.issues.append(LintIssue(path, message))


def is_root_future_work(rel: str) -> bool:
    """True for docs/future-work/<name>.md (not archive/ideas/research)."""
    p = Path(rel.replace("\\", "/"))
    parts = p.parts
    return (
        len(parts) == 3
        and parts[0] == "docs"
        and parts[1] == "future-work"
        and parts[2].endswith(".md")
        and parts[2] != "README.md"
    )


def lint_documents(
    docs: list[DocRecord],
    *,
    active_index_paths: set[str] | None = None,
    committed_generated: dict[str, str] | None = None,
    generated_now: dict[str, str] | None = None,
) -> LintResult:
    """Apply the docs_fixup CI rejection rules to an in-memory document set.

    Rules:
    - root future-work doc without metadata
    - two active docs with same canonical_for
    - implemented/superseded plan listed as active (in active_index_paths)
    - active plan with no open items
    - normative doc that points at archive as authority
    - generated index differs from committed copy
    """
    result = LintResult()
    active_canonical: dict[str, str] = {}

    for doc in docs:
        rel = doc.path.replace("\\", "/")
        if is_root_future_work(rel):
            if doc.meta is None:
                result.add(rel, "root future-work document missing YAML frontmatter metadata")
                continue
            for field_name in REQUIRED_FIELDS:
                if field_name not in doc.meta:
                    result.add(rel, f"missing required metadata field: {field_name}")
            status = str(doc.meta.get("status", ""))
            kind = str(doc.meta.get("kind", ""))
            authority = str(doc.meta.get("authority", ""))
            if status and status not in STATUS_VOCAB:
                result.add(rel, f"invalid status '{status}' (want one of {sorted(STATUS_VOCAB)})")
            if kind and kind not in KIND_VOCAB:
                result.add(rel, f"invalid kind '{kind}' (want one of {sorted(KIND_VOCAB)})")
            if authority and authority not in AUTHORITY_VOCAB:
                result.add(
                    rel,
                    f"invalid authority '{authority}' (want one of {sorted(AUTHORITY_VOCAB)})",
                )

            if status == "active" and kind == "plan":
                open_items = doc.meta.get("open_items")
                if open_items is not True:
                    result.add(
                        rel,
                        "active plan must set open_items: true (completed slices belong elsewhere)",
                    )

            if status in ("implemented", "superseded") and active_index_paths is not None:
                leaf = Path(rel).name
                if rel in active_index_paths or leaf in active_index_paths:
                    result.add(
                        rel,
                        f"{status} plan must not appear in the active future-work index",
                    )

            canon = doc.meta.get("canonical_for")
            if canon and status == "active":
                if canon in active_canonical:
                    result.add(
                        rel,
                        f"duplicate active canonical_for '{canon}' "
                        f"(also claimed by {active_canonical[canon]})",
                    )
                else:
                    active_canonical[canon] = rel

        # Normative must not treat archive as authority
        if doc.meta and str(doc.meta.get("authority")) == "normative":
            for m in CANONICAL_LINK_RE.finditer(doc.body):
                target = m.group(1)
                result.add(
                    rel,
                    f"normative document links to archive path as content: {target}",
                )

    if committed_generated is not None and generated_now is not None:
        for name, expected in generated_now.items():
            actual = committed_generated.get(name)
            if actual is None:
                result.add(name, "generated file missing from tree (run generate and commit)")
            elif _normalize_newlines(actual) != _normalize_newlines(expected):
                result.add(
                    name,
                    "generated index differs from committed copy (run generate and commit)",
                )

    return result


def _normalize_newlines(s: str) -> str:
    return s.replace("\r\n", "\n").replace("\r", "\n")


def parse_active_index_links(readme_text: str) -> set[str]:
    """Paths listed under the Active plans section of future-work/README.md."""
    # Between "## Active plans" and the next ## heading (or end)
    m = re.search(
        r"## Active plans.*?\n(.*?)(?:\n## |\Z)",
        readme_text,
        re.DOTALL | re.IGNORECASE,
    )
    if not m:
        # whole file link targets that look like *.md
        section = readme_text
    else:
        section = m.group(1)
    links = set()
    for target in re.findall(r"\]\(([^)]+\.md)\)", section):
        if target.startswith("http") or "archive/" in target:
            continue
        links.add(Path(target).name)
        links.add(target)
    return links


# ---------------------------------------------------------------------------
# Index generation
# ---------------------------------------------------------------------------


def generate_future_work_readme(docs: list[DocRecord]) -> str:
    """Generate docs/future-work/README.md from root future-work metadata."""
    active: list[tuple[str, dict[str, Any]]] = []
    other: list[tuple[str, dict[str, Any]]] = []
    for doc in docs:
        rel = doc.path.replace("\\", "/")
        if not is_root_future_work(rel):
            continue
        if doc.meta is None:
            continue
        name = Path(rel).name
        if doc.meta.get("status") == "active":
            active.append((name, doc.meta))
        else:
            other.append((name, doc.meta))

    active.sort(key=lambda x: x[0])
    other.sort(key=lambda x: x[0])

    lines = [
        "---",
        "kind: index",
        "status: active",
        "authority: advisory",
        "generated: true",
        "---",
        "",
        "# Future Work",
        "",
        "Index of forward-looking work. **Generated** by `scripts/docs_meta.py generate`.",
        "Do not edit the tables by hand — update document frontmatter and re-run generate.",
        "",
        "| Doc | Role |",
        "|-----|------|",
        "| **[roadmap.md](../roadmap.md)** | **Living scoreboard** — open work in priority order |",
        "| [backlog.md](backlog.md) | **Pick-from-here backlog** — only place agents should take next implementation items |",
        "| [archive/](archive/README.md) | Finished plans, closed audits — **not current truth** |",
        "| [CATALOG.md](../CATALOG.md) | Repository-wide document catalog |",
        "",
        "## Active plans",
        "",
        "Only documents with `status: active` appear here. Implemented and superseded plans are archived.",
        "",
        "| Plan | Kind | Domain | Authority |",
        "|------|------|--------|-----------|",
    ]
    for name, meta in active:
        domain = meta.get("domain", "—")
        lines.append(
            f"| [{name}]({name}) | {meta.get('kind', '—')} | {domain} | {meta.get('authority', '—')} |"
        )
    if not active:
        lines.append("| *(none)* | | | |")

    lines.extend(
        [
            "",
            "## Non-active root documents",
            "",
            "Root files that are not active (historical findings kept briefly, or pending archive).",
            "Prefer archive/ for completed plans.",
            "",
            "| Doc | Status | Kind |",
            "|-----|--------|------|",
        ]
    )
    for name, meta in other:
        lines.append(
            f"| [{name}]({name}) | {meta.get('status', '—')} | {meta.get('kind', '—')} |"
        )
    if not other:
        lines.append("| *(none)* | | |")

    lines.extend(
        [
            "",
            "Start every planning session at [roadmap.md](../roadmap.md).",
            "",
        ]
    )
    return "\n".join(lines)


def generate_catalog(docs: list[DocRecord]) -> str:
    """Generate docs/CATALOG.md from managed documents with frontmatter."""
    rows: list[tuple[str, dict[str, Any]]] = []
    for doc in docs:
        if doc.meta is None:
            continue
        rows.append((doc.path.replace("\\", "/"), doc.meta))
    rows.sort(key=lambda x: x[0])

    lines = [
        "---",
        "kind: index",
        "status: active",
        "authority: advisory",
        "generated: true",
        "---",
        "",
        "# Document catalog",
        "",
        "Repository-wide catalog of managed documents (those with YAML frontmatter).",
        "**Generated** by `scripts/docs_meta.py generate`. Do not edit by hand.",
        "",
        "Authority model: [doc-authority.md](spec/reference/doc-authority.md).",
        "",
        "| Path | Kind | Status | Authority | Domain | Canonical for |",
        "|------|------|--------|-----------|--------|---------------|",
    ]
    for path, meta in rows:
        lines.append(
            "| {path} | {kind} | {status} | {authority} | {domain} | {canon} |".format(
                path=path,
                kind=meta.get("kind", "—"),
                status=meta.get("status", "—"),
                authority=meta.get("authority", "—"),
                domain=meta.get("domain", "—"),
                canon=meta.get("canonical_for", "—"),
            )
        )
    lines.append("")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Repo I/O
# ---------------------------------------------------------------------------


def repo_root_from_script() -> Path:
    return Path(__file__).resolve().parent.parent


def read_text_tolerant(path: Path) -> str:
    """Read markdown as UTF-8, falling back to cp1252 for legacy Windows files."""
    raw = path.read_bytes()
    if raw.startswith(b"\xff\xfe") or raw.startswith(b"\xfe\xff"):
        return raw.decode("utf-16")
    if raw.startswith(b"\xef\xbb\xbf"):
        return raw.decode("utf-8-sig")
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError:
        return raw.decode("cp1252")


def load_docs_from_tree(root: Path) -> list[DocRecord]:
    docs: list[DocRecord] = []
    docs_dir = root / "docs"
    if not docs_dir.is_dir():
        return docs
    # Match .gitignore: working notes that must not enter the committed catalog.
    skip_names = {"session-profiles-next-actions.md"}
    for path in sorted(docs_dir.rglob("*.md")):
        if path.name in skip_names:
            continue
        rel = path.relative_to(root).as_posix()
        text = read_text_tolerant(path)
        meta, body = split_frontmatter(text)
        docs.append(DocRecord(path=rel, meta=meta, body=body))
    return docs


def scan_stale_rs_paths(root: Path) -> list[tuple[str, int, str]]:
    hits: list[tuple[str, int, str]] = []
    crates = root / "crates"
    if not crates.is_dir():
        return hits
    for path in crates.rglob("*.rs"):
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        for i, line in enumerate(text.splitlines(), 1):
            for prefix in OBSOLETE_RS_PREFIXES:
                if prefix in line:
                    hits.append((path.relative_to(root).as_posix(), i, line.strip()))
    return hits


def repair_stale_rs_paths(root: Path) -> int:
    """Rewrite obsolete doc path prefixes in crates/**/*.rs. Returns files changed."""
    changed = 0
    crates = root / "crates"
    for path in crates.rglob("*.rs"):
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        new = text
        for old, new_pref in RS_PATH_REWRITES:
            new = new.replace(old, new_pref)
        if new != text:
            path.write_text(new, encoding="utf-8", newline="\n")
            changed += 1
    return changed


# ---------------------------------------------------------------------------
# Classification defaults for root future-work (bootstrap)
# ---------------------------------------------------------------------------

# status, kind, authority, domain, open_items, canonical_for (optional)
ROOT_CLASSIFICATION: dict[str, dict[str, Any]] = {
    "backlog.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "product",
        "open_items": True,
        "canonical_for": "implementation-backlog",
    },
    "docs_fixup.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "docs",
        "open_items": True,
        "canonical_for": "docs-lifecycle",
    },
    "loops-plan.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "loops",
        "open_items": True,
        "canonical_for": "loops",
    },
    "chat-search-plan.md": {
        "kind": "plan",
        "status": "implemented",
        "authority": "advisory",
        "domain": "chat",
        "open_items": False,
        "canonical_for": "chat-search",
    },
    "context-compaction-plan.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "chat",
        "open_items": True,
        "canonical_for": "context-compaction",
    },
    "context-compaction-viewport-rearchitecture.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "chat",
        "open_items": True,
        "canonical_for": "context-compaction-viewport",
    },
    "delegation-failure-modes.md": {
        "kind": "finding",
        "status": "historical",
        "authority": "evidence",
        "domain": "delegation",
        "open_items": False,
        "canonical_for": "delegation-failure-modes",
    },
    "parallel-deliverables-2026-08.md": {
        "kind": "plan",
        "status": "implemented",
        "authority": "advisory",
        "domain": "process",
        "open_items": False,
        "canonical_for": "parallel-deliverables-r1",
    },
    "parallel-deliverables-2026-08-round-2.md": {
        "kind": "plan",
        "status": "implemented",
        "authority": "advisory",
        "domain": "process",
        "open_items": False,
        "canonical_for": "parallel-deliverables-r2",
    },
    "parallel-deliverables-2026-08-round-3.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "process",
        "open_items": True,
        "canonical_for": "parallel-deliverables-r3",
    },
    "token-economics-findings-2026-08.md": {
        "kind": "finding",
        "status": "active",
        "authority": "evidence",
        "domain": "token-economics",
        "open_items": True,
        "canonical_for": "token-economics-findings",
    },
    "token-cost-accounting-plan.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "token-economics",
        "open_items": True,
        "canonical_for": "token-cost-accounting",
    },
    "delegated-work-is-discarded-at-the-seam.md": {
        "kind": "finding",
        "status": "implemented",
        "authority": "evidence",
        "domain": "delegation",
        "open_items": False,
        "canonical_for": "delegated-work-seam",
    },
    "build-locally-and-ship-the-artifact.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "ops",
        "open_items": True,
        "canonical_for": "local-build-ship",
    },
    "coding-tui-plan.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "coding-harness",
        "open_items": True,
        "canonical_for": "coding-tui",
    },
    "rust-native-agentic-coder-plan.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "coding-harness",
        "open_items": True,
        "canonical_for": "agentic-mesh-coding-pack",
    },
    "tui-maturity-roadmap.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "tui",
        "open_items": True,
        "canonical_for": "tui-maturity",
    },
    "turbovault-modules-integration-roadmap.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "turbovault",
        "open_items": True,
        "canonical_for": "turbovault-modules",
    },
    "turbovault-vault-events-plugin-plan.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "turbovault",
        "open_items": True,
        "canonical_for": "turbovault-vault-events",
    },
    "mcp-forge-backlog.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "mcp",
        "open_items": True,
        "canonical_for": "mcp-forge-backlog",
    },
    "mcp-suite-standardization.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "mcp",
        "open_items": True,
        "canonical_for": "mcp-suite-standardization",
    },
    "latency-and-routing-observability-plan.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "observability",
        "open_items": True,
        "canonical_for": "latency-routing-observability",
    },
    "heuristics-tuning-engine-plan.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "tuning",
        "open_items": True,
        "canonical_for": "heuristics-tuning-engine",
    },
    "coder-eval-curriculum.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "coding-harness",
        "open_items": True,
        "canonical_for": "coder-eval-curriculum",
    },
    "pr-dispatch-vtcode-no-write-finding.md": {
        "kind": "finding",
        "status": "active",
        "authority": "evidence",
        "domain": "coding-harness",
        "open_items": True,
        "canonical_for": "vtcode-no-write",
    },
    "self-host-coding-dogfood-2026-08.md": {
        "kind": "finding",
        "status": "active",
        "authority": "evidence",
        "domain": "coding-harness",
        "open_items": True,
        "canonical_for": "self-host-dogfood-2026-08",
    },
    "self-pr-quality-roadmap.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "coding-harness",
        "open_items": True,
        "canonical_for": "self-pr-quality",
    },
    "paseo-liberado-integration-roadmap.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "acp",
        "open_items": True,
        "canonical_for": "paseo-liberado-integration",
    },
    "acp-bridge-completion-roadmap.md": {
        "kind": "plan",
        "status": "superseded",
        "authority": "advisory",
        "domain": "acp",
        "open_items": False,
        "canonical_for": "acp-bridge-completion",
        "superseded_by": "paseo-liberado-integration-roadmap.md",
    },
    "live-conformance-suite.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "conformance",
        "open_items": True,
        "canonical_for": "live-conformance",
    },
    "live-conformance-tier3-build-spec.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "conformance",
        "open_items": True,
        "canonical_for": "live-conformance-tier3",
    },
    "coder-harness-reliability-2026-08.md": {
        "kind": "finding",
        "status": "active",
        "authority": "evidence",
        "domain": "coding-harness",
        "open_items": True,
        "canonical_for": "coder-harness-reliability-2026-08",
    },
    "harness-study-2026-08.md": {
        "kind": "finding",
        "status": "active",
        "authority": "advisory",
        "domain": "coding-harness",
        "open_items": True,
        "canonical_for": "harness-study-2026-08",
    },
    "harness-bench-gaps-and-levers.md": {
        "kind": "finding",
        "status": "active",
        "authority": "advisory",
        "domain": "coding-harness",
        "open_items": True,
        "canonical_for": "harness-bench-gaps",
    },
    "model-knob-profiles.md": {
        "kind": "plan",
        "status": "draft",
        "authority": "advisory",
        "domain": "tuning",
        "open_items": False,
        "canonical_for": "model-knob-profiles",
    },
    "cadence-triggered-maintenance-agents.md": {
        "kind": "plan",
        "status": "draft",
        "authority": "advisory",
        "domain": "ops",
        "open_items": False,
        "canonical_for": "cadence-maintenance-agents",
    },
    "session-profiles-next-actions.md": {
        "kind": "plan",
        "status": "active",
        "authority": "implementation",
        "domain": "session",
        "open_items": True,
        "canonical_for": "session-profiles-next",
    },
}

# Fully landed plans: move to archive after metadata + distillation note.
ARCHIVE_CANDIDATES = [
    "parallel-deliverables-2026-08.md",
    "parallel-deliverables-2026-08-round-2.md",
    "acp-bridge-completion-roadmap.md",
    "chat-search-plan.md",
    "delegated-work-is-discarded-at-the-seam.md",
]


def apply_root_metadata(root: Path) -> int:
    """Add or merge frontmatter on root future-work docs. Returns count updated."""
    fw = root / "docs" / "future-work"
    updated = 0
    for path in sorted(fw.glob("*.md")):
        if path.name == "README.md":
            continue
        meta = ROOT_CLASSIFICATION.get(path.name)
        if meta is None:
            # Default: active plan with open items so lint fails closed only if missing entirely
            meta = {
                "kind": "plan",
                "status": "active",
                "authority": "implementation",
                "domain": "uncategorized",
                "open_items": True,
            }
        text = read_text_tolerant(path)
        new_text = ensure_frontmatter(text, meta)
        if new_text != text:
            path.write_text(new_text, encoding="utf-8", newline="\n")
            updated += 1
    return updated


def archive_completed_plans(root: Path) -> list[str]:
    """Move ARCHIVE_CANDIDATES into future-work/archive/. Returns moved names."""
    fw = root / "docs" / "future-work"
    archive = fw / "archive"
    archive.mkdir(exist_ok=True)
    moved: list[str] = []
    for name in ARCHIVE_CANDIDATES:
        src = fw / name
        if not src.is_file():
            continue
        dest = archive / name
        if dest.exists():
            continue
        text = read_text_tolerant(src)
        meta, body = split_frontmatter(text)
        if meta is None:
            meta = {}
        # Ensure terminal status when archiving
        if meta.get("status") not in ("implemented", "superseded", "historical"):
            meta["status"] = "implemented"
        meta["kind"] = meta.get("kind", "plan")
        meta["authority"] = meta.get("authority", "advisory")
        meta["open_items"] = False
        banner = (
            "> **Archived.** This plan is not current truth. "
            "Open work lives in [backlog.md](../backlog.md) and [roadmap.md](../../roadmap.md). "
            "See [doc-authority.md](../../spec/reference/doc-authority.md).\n\n"
        )
        if not body.lstrip().startswith("> **Archived."):
            body = banner + body.lstrip()
        dest.write_text(dump_frontmatter(meta) + "\n" + body, encoding="utf-8", newline="\n")
        src.unlink()
        moved.append(name)
    return moved


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def cmd_lint(root: Path) -> int:
    docs = load_docs_from_tree(root)
    readme_path = root / "docs" / "future-work" / "README.md"
    catalog_path = root / "docs" / "CATALOG.md"
    active_links: set[str] = set()
    if readme_path.is_file():
        active_links = parse_active_index_links(readme_path.read_text(encoding="utf-8"))

    generated_now = {
        "docs/future-work/README.md": generate_future_work_readme(docs),
        "docs/CATALOG.md": generate_catalog(docs),
    }
    committed = {}
    if readme_path.is_file():
        committed["docs/future-work/README.md"] = readme_path.read_text(encoding="utf-8")
    if catalog_path.is_file():
        committed["docs/CATALOG.md"] = catalog_path.read_text(encoding="utf-8")

    result = lint_documents(
        docs,
        active_index_paths=active_links,
        committed_generated=committed,
        generated_now=generated_now,
    )

    # Also require every root future-work file present
    fw = root / "docs" / "future-work"
    skip_names = {"session-profiles-next-actions.md"}
    if fw.is_dir():
        for path in fw.glob("*.md"):
            if path.name in ("README.md",) or path.name in skip_names:
                continue
            rel = path.relative_to(root).as_posix()
            if not any(d.path.replace("\\", "/") == rel for d in docs):
                result.add(rel, "unreadable root future-work document")

    if result.ok:
        print("docs_meta lint: OK")
        return 0
    print("docs_meta lint: FAILED", file=sys.stderr)
    for issue in result.issues:
        print(f"  {issue.path}: {issue.message}", file=sys.stderr)
    return 1


def cmd_generate(root: Path) -> int:
    # Two passes so the catalog includes the generated files themselves (fixed point).
    readme_path = root / "docs" / "future-work" / "README.md"
    catalog_path = root / "docs" / "CATALOG.md"
    for _ in range(2):
        docs = load_docs_from_tree(root)
        readme = generate_future_work_readme(docs)
        catalog = generate_catalog(docs)
        readme_path.write_text(readme, encoding="utf-8", newline="\n")
        catalog_path.write_text(catalog, encoding="utf-8", newline="\n")
    print(f"wrote {readme_path.relative_to(root).as_posix()}")
    print(f"wrote {catalog_path.relative_to(root).as_posix()}")
    return 0


def cmd_check_stale_rs(root: Path) -> int:
    hits = scan_stale_rs_paths(root)
    if not hits:
        print("stale-rs-paths: OK (no obsolete docs/architecture or docs/roadmap refs)")
        return 0
    print("stale-rs-paths: FAILED", file=sys.stderr)
    for path, line, text in hits[:50]:
        print(f"  {path}:{line}: {text}", file=sys.stderr)
    if len(hits) > 50:
        print(f"  ... and {len(hits) - 50} more", file=sys.stderr)
    return 1


def cmd_apply_metadata(root: Path) -> int:
    n = apply_root_metadata(root)
    print(f"updated frontmatter on {n} root future-work documents")
    return 0


def cmd_archive(root: Path) -> int:
    moved = archive_completed_plans(root)
    for name in moved:
        print(f"archived {name}")
    if not moved:
        print("no candidates to archive (already moved or missing)")
    return 0


def cmd_repair_rs(root: Path) -> int:
    n = repair_stale_rs_paths(root)
    print(f"rewrote obsolete doc paths in {n} .rs files")
    return 0


def cmd_self_test() -> int:
    """Pure unit tests for lint rules — no monorepo required."""
    failures = 0

    def check(name: str, cond: bool) -> None:
        nonlocal failures
        if cond:
            print(f"  ok  {name}")
        else:
            print(f"  FAIL {name}")
            failures += 1

    # Missing metadata on root future-work
    docs = [
        DocRecord("docs/future-work/foo.md", None, "# Foo\n"),
    ]
    r = lint_documents(docs)
    check("rejects missing frontmatter", any("missing YAML" in i.message for i in r.issues))

    # Active plan without open_items
    docs = [
        DocRecord(
            "docs/future-work/foo.md",
            {"kind": "plan", "status": "active", "authority": "implementation", "open_items": False},
            "",
        )
    ]
    r = lint_documents(docs)
    check("rejects active plan without open_items", any("open_items" in i.message for i in r.issues))

    # Duplicate canonical_for
    docs = [
        DocRecord(
            "docs/future-work/a.md",
            {
                "kind": "plan",
                "status": "active",
                "authority": "implementation",
                "open_items": True,
                "canonical_for": "same",
            },
            "",
        ),
        DocRecord(
            "docs/future-work/b.md",
            {
                "kind": "plan",
                "status": "active",
                "authority": "implementation",
                "open_items": True,
                "canonical_for": "same",
            },
            "",
        ),
    ]
    r = lint_documents(docs)
    check("rejects duplicate active canonical_for", any("duplicate" in i.message for i in r.issues))

    # implemented in active index
    docs = [
        DocRecord(
            "docs/future-work/done.md",
            {"kind": "plan", "status": "implemented", "authority": "advisory", "open_items": False},
            "",
        )
    ]
    r = lint_documents(docs, active_index_paths={"done.md"})
    check(
        "rejects implemented plan in active index",
        any("must not appear" in i.message for i in r.issues),
    )

    # normative linking to archive
    docs = [
        DocRecord(
            "docs/spec/architecture/contracts.md",
            {"kind": "architecture", "status": "active", "authority": "normative"},
            "See [old](../future-work/archive/old-plan.md) for rules.\n",
        )
    ]
    r = lint_documents(docs)
    check("rejects normative→archive link", any("archive" in i.message for i in r.issues))

    # generated differs
    docs = [
        DocRecord(
            "docs/future-work/x.md",
            {
                "kind": "plan",
                "status": "active",
                "authority": "implementation",
                "open_items": True,
                "domain": "t",
            },
            "",
        )
    ]
    gen = generate_future_work_readme(docs)
    r = lint_documents(
        docs,
        committed_generated={"docs/future-work/README.md": "stale\n"},
        generated_now={"docs/future-work/README.md": gen},
    )
    check(
        "rejects stale generated index",
        any("differs from committed" in i.message for i in r.issues),
    )

    # happy path
    docs = [
        DocRecord(
            "docs/future-work/x.md",
            {
                "kind": "plan",
                "status": "active",
                "authority": "implementation",
                "open_items": True,
                "domain": "t",
                "canonical_for": "x",
            },
            "",
        )
    ]
    gen = generate_future_work_readme(docs)
    cat = generate_catalog(docs)
    r = lint_documents(
        docs,
        active_index_paths={"x.md"},
        committed_generated={
            "docs/future-work/README.md": gen,
            "docs/CATALOG.md": cat,
        },
        generated_now={
            "docs/future-work/README.md": gen,
            "docs/CATALOG.md": cat,
        },
    )
    check("accepts valid active plan + matching generated", r.ok)

    # frontmatter round-trip
    meta = {"kind": "plan", "status": "active", "authority": "implementation", "open_items": True}
    text = dump_frontmatter(meta) + "\n# Title\n"
    parsed, body = split_frontmatter(text)
    check("frontmatter round-trip kind", parsed is not None and parsed.get("kind") == "plan")
    check("frontmatter round-trip open_items", parsed is not None and parsed.get("open_items") is True)
    check("body preserved", body.strip().startswith("# Title"))

    if failures:
        print(f"self-test: {failures} failure(s)")
        return 1
    print("self-test: all passed")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "command",
        choices=[
            "lint",
            "generate",
            "check-stale-rs",
            "apply-metadata",
            "archive",
            "repair-rs",
            "self-test",
        ],
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=None,
        help="Repository root (default: parent of scripts/)",
    )
    args = parser.parse_args(argv)
    if args.command == "self-test":
        return cmd_self_test()
    root = (args.root or repo_root_from_script()).resolve()
    if args.command == "lint":
        return cmd_lint(root)
    if args.command == "generate":
        return cmd_generate(root)
    if args.command == "check-stale-rs":
        return cmd_check_stale_rs(root)
    if args.command == "apply-metadata":
        return cmd_apply_metadata(root)
    if args.command == "archive":
        return cmd_archive(root)
    if args.command == "repair-rs":
        return cmd_repair_rs(root)
    return 2


if __name__ == "__main__":
    sys.exit(main())
