#!/usr/bin/env python3
"""Compatibility entry point for the native docs-site generator."""

from __future__ import annotations

import os
import subprocess
import sys


def main() -> int:
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    return subprocess.call(
        [
            "cargo",
            "run",
            "--locked",
            "-p",
            "liberado-cli",
            "--",
            "docs",
            "site",
            *sys.argv[1:],
        ],
        cwd=root,
    )


if __name__ == "__main__":
    sys.exit(main())
