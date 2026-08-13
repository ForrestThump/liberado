#!/usr/bin/env python3
"""High-signal summary of a compare-run log. Stdlib only.

This is the report an agent otherwise rebuilds with throwaway Python every time:
turns, first edit, tools, ship-bar, usage, cargo commands, last model text.

Usage:
  python scripts/summarize-compare-run.py PATH
  python scripts/summarize-compare-run.py --dir DIR

PATH may be:
  * a Liberado CoderEvent .json (or its sibling .mvl.jsonl)
  * a Liberado coder-traces directory
  * a pi session.jsonl
  * a deepagents run.mvl.jsonl
  * a compare out/ directory (liberado/, pi/, deepagents/)
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from collections import Counter
from datetime import datetime
from pathlib import Path


def parse_ts(raw: str | None) -> datetime | None:
    if not raw:
        return None
    raw = raw.replace("Z", "+00:00")
    if "." in raw:
        head, rest = raw.split(".", 1)
        frac = rest.split("+")[0].split("-")[0]
        tz = rest[len(frac) :]
        raw = f"{head}.{frac[:6]}{tz}"
    try:
        return datetime.fromisoformat(raw)
    except ValueError:
        return None


def secs(a: datetime | None, b: datetime | None) -> float | None:
    if a is None or b is None:
        return None
    return (b - a).total_seconds()


def fmt_secs(n: float | None) -> str:
    if n is None:
        return "?"
    if n < 90:
        return f"{n:.0f}s"
    return f"{n / 60:.0f} min {n % 60:.0f}s"


def load_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def iter_jsonl(path: Path):
    with path.open(encoding="utf-8", errors="replace") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                yield json.loads(line)
            except json.JSONDecodeError:
                continue


def detect(path: Path) -> str:
    if path.is_dir():
        if (path / "session.jsonl").exists() or (path / "run.mvl.jsonl").exists():
            return "outdir"
        if (path / "liberado").is_dir() or (path / "pi").is_dir():
            return "compare"
        jsons = list(path.glob("*.json"))
        if any(not p.name.endswith("execution.json") for p in jsons):
            return "liberado-dir"
        return "dir"
    name = path.name.lower()
    if name.endswith(".mvl.jsonl"):
        return "mvl"
    if name == "session.jsonl":
        return "pi"
    if name.endswith(".json") and not name.endswith("execution.json"):
        return "liberado-json"
    if name.endswith(".jsonl"):
        # peek
        for obj in iter_jsonl(path):
            t = obj.get("type")
            if t in ("session", "turn_start", "agent_start"):
                return "pi"
            if t in ("run_started", "completion", "tool_result"):
                return "mvl"
            break
        return "jsonl"
    return "unknown"


def summarize_liberado_json(path: Path) -> None:
    data = load_json(path)
    req = data.get("request") or {}
    cfg = (req.get("config") or {}).get("coder") or {}
    evs = data.get("events") or []
    t0 = parse_ts(evs[0].get("at")) if evs else None
    t1 = parse_ts(evs[-1].get("at")) if evs else None
    turn = 0
    tools: Counter[str] = Counter()
    first_edit = None
    print(f"## Liberado  {path.name}")
    print(f"- attempt: {req.get('attempt')}   max_turns: {cfg.get('max_turns')}   model: {cfg.get('model')}")
    print(f"- reasoning: {cfg.get('reasoning')}   wall: {fmt_secs(secs(t0, t1))}")
    for e in evs:
        kind = e.get("type")
        if kind == "model_turn_finished":
            turn += 1
        if kind == "tool_started":
            name = e.get("tool") or e.get("name") or "?"
            tools[name] += 1
            if name in ("edit_file", "write_file", "apply_patch", "hashline_edit") and first_edit is None:
                first_edit = turn
        if kind == "loop_guard_triggered":
            print(f"- guard ~turn {turn}: {e.get('guard')} {e.get('action')}")
        if kind in ("report_filed", "validation_finished", "session_finished"):
            extra = e.get("outcome") or e.get("summary") or ""
            print(f"- {kind}: {str(extra)[:160]}")
    print(f"- turns: {turn}   first mutation: turn {first_edit}   tools: {dict(tools)}")
    mvl = path.with_suffix(".mvl.jsonl")
    if not mvl.exists():
        # sibling named the same stem
        cand = path.parent / (path.stem + ".mvl.jsonl")
        mvl = cand if cand.exists() else mvl
    if mvl.exists():
        summarize_mvl(mvl, heading=False)


def summarize_mvl(path: Path, heading: bool = True) -> None:
    usage: Counter[str] = Counter()
    tools: Counter[str] = Counter()
    types: Counter[str] = Counter()
    cargo: list[str] = []
    last_text = ""
    last_finish = ""
    n_comp = 0
    first_edit_turn = None
    first_edit_path = ""
    for obj in iter_jsonl(path):
        t = obj.get("type")
        types[t] += 1
        if t == "completion":
            n_comp += 1
            u = obj.get("usage") or {}
            for k, v in u.items():
                if isinstance(v, (int, float)):
                    usage[k] += v
            text = obj.get("text") or ""
            if text.strip():
                last_text = text
            last_finish = obj.get("finish_reason") or last_finish
            for tc in obj.get("tool_calls") or []:
                name = tc.get("name") or "?"
                tools[name] += 1
                args = tc.get("arguments") or {}
                if name in ("edit_file", "write_file", "edit", "write") and first_edit_turn is None:
                    first_edit_turn = obj.get("turn")
                    first_edit_path = str(args.get("path") or args.get("file_path") or "")
                if name == "run_command":
                    prog = args.get("program")
                    a = args.get("args")
                    if prog in ("cargo", "git") or (
                        isinstance(a, list) and a and str(a[0]) in ("check", "test", "clippy")
                    ):
                        cargo.append(f"t{obj.get('turn')} {prog} {a}")
                if name == "bash":
                    cmd = args.get("command") or ""
                    if "cargo" in str(cmd):
                        cargo.append(f"t{obj.get('turn')} {str(cmd)[:140]}")
        if t == "tool_result":
            shown = obj.get("content_shown") or obj.get("full_content") or ""
            shown = shown.replace("\\n", "\n").replace("\\u001b", "\x1b")
            for line in shown.splitlines():
                clean = re.sub(r"\x1b\[[0-9;]*m", "", line)
                if re.search(r"error\[E\d+\]|test \S+ \.\.\. FAILED", clean):
                    cargo.append(f"  fail t{obj.get('turn')}: {clean.strip()[:160]}")
    if heading:
        print(f"## MVL  {path.name}")
    print(f"- mvl completions: {n_comp}   finish: {last_finish}   first edit turn: {first_edit_turn}")
    if usage:
        print(
            "- usage: "
            + ", ".join(f"{k}={v:,.0f}" for k, v in usage.items())
        )
    if tools:
        print(f"- tool calls: {dict(tools)}")
    if first_edit_path:
        print(f"- first edit path: {first_edit_path}")
    if cargo:
        print("- cargo / named failures:")
        for line in cargo[:40]:
            print(f"  {line}")
        if len(cargo) > 40:
            print(f"  … {len(cargo) - 40} more")
    if last_text.strip():
        clip = last_text.strip().replace("\n", " ")
        print(f"- last completion: {clip[:280]}")


def summarize_pi(path: Path) -> None:
    turns = 0
    tools: Counter[str] = Counter()
    first_edit = None
    last_text = ""
    cargo: list[str] = []
    timeouts = 0
    for obj in iter_jsonl(path):
        t = obj.get("type")
        if t == "turn_start":
            turns += 1
        if t == "tool_execution_start":
            name = obj.get("toolName") or obj.get("name") or "?"
            tools[name] += 1
            args = obj.get("args") or obj.get("input") or {}
            path_arg = args.get("path") if isinstance(args, dict) else None
            cmd = args.get("command") if isinstance(args, dict) else None
            if name in ("edit", "write") and first_edit is None:
                first_edit = (turns, path_arg)
            if name == "bash" and cmd and "cargo" in str(cmd):
                cargo.append(f"t{turns} {str(cmd)[:140]}")
        if t == "message_end":
            msg = obj.get("message") or {}
            if msg.get("role") == "assistant":
                content = msg.get("content")
                texts = []
                if isinstance(content, list):
                    for c in content:
                        if isinstance(c, dict) and c.get("type") == "text" and c.get("text"):
                            texts.append(c["text"])
                if texts:
                    last_text = "".join(texts)
        blob = json.dumps(obj).lower()
        if "connect timeout" in blob:
            timeouts += 1
    print(f"## pi  {path.name}")
    print(f"- turns: {turns}   tools: {dict(tools)}")
    print(f"- first edit: {first_edit}")
    print(f"- connect-timeout mentions: {timeouts}")
    if cargo:
        print("- cargo:")
        for line in cargo[:30]:
            print(f"  {line}")
        if len(cargo) > 30:
            print(f"  … {len(cargo) - 30} more")
    if last_text:
        print(f"- last assistant: {last_text.strip().replace(chr(10), ' ')[:320]}")


def git_stat(ws: Path) -> None:
    if not (ws / ".git").exists() and not (ws / ".git").is_file():
        # linked worktree uses .git file
        if not (ws / ".git").exists():
            return
    try:
        st = subprocess.check_output(
            ["git", "-C", str(ws), "status", "-sb"],
            text=True,
            encoding="utf-8",
            errors="replace",
        )
        diff = subprocess.check_output(
            ["git", "-C", str(ws), "diff", "--stat"],
            text=True,
            encoding="utf-8",
            errors="replace",
        )
    except (OSError, subprocess.CalledProcessError) as e:
        print(f"- git: {e}")
        return
    print(f"## git  {ws}")
    print(st.rstrip() or "(clean)")
    if diff.strip():
        print(diff.rstrip())


def walk(path: Path) -> None:
    kind = detect(path)
    if kind == "liberado-json":
        summarize_liberado_json(path)
    elif kind == "mvl":
        summarize_mvl(path)
    elif kind == "pi":
        summarize_pi(path)
    elif kind == "liberado-dir":
        files = sorted(
            p
            for p in path.glob("*.json")
            if not p.name.endswith("execution.json")
        )
        for p in files:
            summarize_liberado_json(p)
            print()
    elif kind in ("outdir", "compare", "dir"):
        # common compare layouts
        lib_traces = path / "liberado" / "traces"
        if not lib_traces.exists():
            # traces left in the worktree
            pass
        for label, rel in (
            ("liberado traces", path / "liberado" / "traces"),
            ("liberado traces", path / "traces"),
            ("pi", path / "pi" / "session.jsonl"),
            ("pi", path / "session.jsonl"),
            ("deepagents", path / "deepagents" / "run.mvl.jsonl"),
            ("deepagents", path / "run.mvl.jsonl"),
        ):
            if rel.exists():
                print(f"# {label}")
                walk(rel)
                print()
    else:
        print(f"unrecognized: {path} ({kind})", file=sys.stderr)
        sys.exit(2)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("path", type=Path, help="trace file or directory")
    ap.add_argument(
        "--git",
        type=Path,
        default=None,
        help="also print git status --short / diff --stat for this worktree",
    )
    args = ap.parse_args()
    path = args.path
    if not path.exists():
        print(f"not found: {path}", file=sys.stderr)
        return 2
    walk(path)
    if args.git:
        print()
        git_stat(args.git)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
