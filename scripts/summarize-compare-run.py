#!/usr/bin/env python3
\"\"\"Compatibility entry point for liberado coder summarize.\"\"\"

from __future__ import annotations

import os
import subprocess
import sys


def main() -> int:
    if not sys.argv[1:]:
        print(\"usage: summarize-compare-run.py <path> [--git <workspace>]\", file=sys.stderr)
        return 2
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    return subprocess.call(
        [\"cargo\", \"run\", \"--locked\", \"-p\", \"liberado-cli\", \"--\", \"coder\", \"summarize\", *sys.argv[1:]],
        cwd=root,
    )


if __name__ == \"__main__\":
    raise SystemExit(main())
