#!/usr/bin/env python3
"""Compatibility entry point for the native documentation metadata commands.

Use ``liberado docs metadata <command>`` directly. This filename remains so
older local instructions fail over to the same Rust implementation.
"""

from __future__ import annotations

import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any


@dataclass
class DocRecord:
    """Small read-only compatibility type for older local integrations."""

    path: str
    meta: dict[str, Any] | None
    body: str


def configure_stdio() -> None:
    """Keep this legacy compatibility module safe on Windows consoles."""
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if callable(reconfigure):
            reconfigure(encoding="utf-8", errors="replace")


def safe_print(*args: object, file=None, **kwargs: object) -> None:
    configure_stdio()
    print(*args, file=file or sys.stdout, **kwargs)


def _split_frontmatter(text: str) -> tuple[dict[str, Any] | None, str]:
    text = text.replace("\r\n", "\n")
    if not text.startswith("---\n"):
        return None, text
    marker = text.find("\n---\n", 4)
    if marker < 0:
        return None, text
    metadata: dict[str, Any] = {}
    for line in text[4:marker].splitlines():
        if ":" not in line:
            continue
        key, value = line.split(":", 1)
        value = value.strip().strip("\"'")
        if value.lower() in ("true", "false"):
            metadata[key.strip()] = value.lower() == "true"
        else:
            metadata[key.strip()] = value
    return metadata, text[marker + 6 :]


def load_docs_from_tree(root: Path) -> list[DocRecord]:
    """Read Markdown for older integrations that imported this helper."""
    docs: list[DocRecord] = []
    for path in sorted((root / "docs").rglob("*.md")):
        if path.name == "session-profiles-next-actions.md":
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        meta, body = _split_frontmatter(text)
        docs.append(DocRecord(path.relative_to(root).as_posix(), meta, body))
    return docs


def main() -> int:
    command = sys.argv[1:]
    if not command or command[0].startswith("-"):
        print("usage: docs_meta.py <lint|generate|check-stale-rs|self-test>", file=sys.stderr)
        return 2
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    return subprocess.call(
        ["cargo", "run", "--locked", "-p", "liberado-cli", "--", "docs", "metadata", *command],
        cwd=root,
    )


if __name__ == "__main__":
    sys.exit(main())
