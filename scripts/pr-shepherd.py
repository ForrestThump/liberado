#!/usr/bin/env python3
"""Compatibility launcher for the Rust-native PR shepherd.

The implementation is `liberado shepherd`. This file remains only for existing
automation that still invokes the historical Python path.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent


def main() -> int:
    return subprocess.call(
        ["cargo", "run", "--locked", "-p", "liberado-cli", "--", "shepherd", *sys.argv[1:]],
        cwd=ROOT,
    )


if __name__ == "__main__":
    raise SystemExit(main())
