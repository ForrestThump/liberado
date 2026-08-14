#!/usr/bin/env python3
"""Compatibility entry point for the native documentation metadata commands.

Use ``liberado docs metadata <command>`` directly. This filename remains so
older local instructions fail over to the same Rust implementation.
"""

from __future__ import annotations

import os
import subprocess
import sys


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
