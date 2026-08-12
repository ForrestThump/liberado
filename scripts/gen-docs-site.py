#!/usr/bin/env python3
"""Generate a searchable wiki-like documentation site from repository Markdown.

Outputs under `docs-site/` (gitignored build artifact) or a path given by --out:

  - SUMMARY.md suitable for mdBook
  - index.html with client-side full-text search over document titles/paths/status
  - status and domain indexes
  - backlinks and supersedes edges derived from frontmatter + markdown links
  - pointers to crate Rustdoc (target/doc) and ADRs

Does not introduce a second editable source of truth — all content is derived
from the repository tree.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import defaultdict
from html import escape
from pathlib import Path

# Reuse frontmatter helpers
sys.path.insert(0, str(Path(__file__).resolve().parent))
from docs_meta import (  # noqa: E402
    is_root_future_work,
    load_docs_from_tree,
    parse_simple_yaml,
    split_frontmatter,
)

LINK_RE = re.compile(r"\[([^\]]*)\]\(([^)]+)\)")


def collect_links(body: str, from_path: str) -> list[str]:
    """Return repo-relative targets linked from body."""
    base = Path(from_path).parent
    out: list[str] = []
    for _, target in LINK_RE.findall(body):
        t = target.split("#")[0].strip()
        if not t or t.startswith(("http://", "https://", "mailto:", "//")):
            continue
        if t.endswith("/"):
            continue
        # resolve relative
        try:
            resolved = (base / t).as_posix()
            # normalize ..
            parts: list[str] = []
            for p in resolved.split("/"):
                if p == "..":
                    if parts:
                        parts.pop()
                elif p in ("", "."):
                    continue
                else:
                    parts.append(p)
            out.append("/".join(parts))
        except Exception:
            continue
    return out


def build_search_index(docs) -> list[dict]:
    entries = []
    for doc in docs:
        meta = doc.meta or {}
        title = ""
        for line in doc.body.splitlines():
            if line.startswith("# "):
                title = line[2:].strip()
                break
        if not title:
            title = Path(doc.path).stem
        entries.append(
            {
                "path": doc.path,
                "title": title,
                "kind": meta.get("kind", ""),
                "status": meta.get("status", ""),
                "authority": meta.get("authority", ""),
                "domain": meta.get("domain", ""),
                "canonical_for": meta.get("canonical_for", ""),
                "supersedes": meta.get("supersedes", []),
                "superseded_by": meta.get("superseded_by", ""),
            }
        )
    return entries


def write_summary(docs, out: Path) -> None:
    lines = ["# Summary", "", "[Home](README.md)", ""]
    by_kind: dict[str, list] = defaultdict(list)
    for doc in docs:
        kind = (doc.meta or {}).get("kind", "other")
        by_kind[kind].append(doc)
    for kind in sorted(by_kind):
        lines.append(f"## {kind}")
        lines.append("")
        for doc in sorted(by_kind[kind], key=lambda d: d.path):
            title = Path(doc.path).name
            # mdBook paths relative to docs/
            rel = doc.path
            if rel.startswith("docs/"):
                rel = rel[len("docs/") :]
            lines.append(f"- [{title}]({rel})")
        lines.append("")
    (out / "SUMMARY.md").write_text("\n".join(lines), encoding="utf-8", newline="\n")


def write_html(entries: list[dict], backlinks: dict[str, list[str]], out: Path) -> None:
    index_json = json.dumps(entries, indent=None)
    backlinks_json = json.dumps(backlinks, indent=None)
    html = f"""<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8"/>
  <title>Liberado docs</title>
  <style>
    body {{ font-family: system-ui, sans-serif; margin: 1.5rem; max-width: 960px; }}
    input {{ width: 100%; padding: 0.5rem; font-size: 1rem; }}
    .meta {{ color: #555; font-size: 0.85rem; }}
    li {{ margin: 0.4rem 0; }}
    code {{ font-size: 0.9em; }}
  </style>
</head>
<body>
  <h1>Liberado documentation (generated)</h1>
  <p>Full-text filter over managed document titles, paths, status, domain, and authority.
  Source of truth remains the git repository — this page is a view.</p>
  <p class="meta">Rustdoc (after <code>cargo doc --workspace --no-deps</code>):
  <code>target/doc/&lt;crate&gt;/index.html</code>. ADRs:
  <a href="../docs/decisions/README.md">docs/decisions/</a>.
  Authority: <a href="../docs/spec/reference/doc-authority.md">doc-authority.md</a>.</p>
  <input id="q" type="search" placeholder="Search title, path, status, domain…" autofocus/>
  <p id="count" class="meta"></p>
  <ul id="results"></ul>
  <h2>Status index</h2>
  <ul id="by-status"></ul>
  <h2>Domain index</h2>
  <ul id="by-domain"></ul>
  <script>
    const DOCS = {index_json};
    const BACKLINKS = {backlinks_json};
    const results = document.getElementById('results');
    const count = document.getElementById('count');
    function render(list) {{
      results.innerHTML = '';
      count.textContent = list.length + ' document(s)';
      for (const d of list) {{
        const li = document.createElement('li');
        const bl = (BACKLINKS[d.path] || []).slice(0, 5).join(', ');
        li.innerHTML = '<strong>' + escapeHtml(d.title) + '</strong> ' +
          '<span class="meta">[' + escapeHtml(d.status || '?') + ' · ' +
          escapeHtml(d.kind || '?') + ' · ' + escapeHtml(d.authority || '?') +
          (d.domain ? ' · ' + escapeHtml(d.domain) : '') + ']</span><br/>' +
          '<code>' + escapeHtml(d.path) + '</code>' +
          (d.superseded_by ? '<br/><span class="meta">superseded by: ' +
            escapeHtml(String(d.superseded_by)) + '</span>' : '') +
          (bl ? '<br/><span class="meta">backlinks: ' + escapeHtml(bl) + '</span>' : '');
        results.appendChild(li);
      }}
    }}
    function escapeHtml(s) {{
      return String(s).replace(/[&<>"']/g, c => ({{
        '&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'
      }})[c]);
    }}
    function filter(q) {{
      q = q.trim().toLowerCase();
      if (!q) return DOCS;
      return DOCS.filter(d =>
        (d.title + ' ' + d.path + ' ' + d.status + ' ' + d.kind + ' ' +
         d.domain + ' ' + d.authority + ' ' + d.canonical_for).toLowerCase().includes(q)
      );
    }}
    document.getElementById('q').addEventListener('input', e => render(filter(e.target.value)));
    render(DOCS);
    // status / domain indexes
    function group(key) {{
      const m = {{}};
      for (const d of DOCS) {{
        const k = d[key] || '(none)';
        (m[k] = m[k] || []).push(d);
      }}
      return m;
    }}
    function fillGroup(id, key) {{
      const ul = document.getElementById(id);
      const m = group(key);
      for (const k of Object.keys(m).sort()) {{
        const li = document.createElement('li');
        li.innerHTML = '<strong>' + escapeHtml(k) + '</strong> (' + m[k].length + '): ' +
          m[k].slice(0, 8).map(d => '<code>' + escapeHtml(d.path) + '</code>').join(', ') +
          (m[k].length > 8 ? '…' : '');
        ul.appendChild(li);
      }}
    }}
    fillGroup('by-status', 'status');
    fillGroup('by-domain', 'domain');
  </script>
</body>
</html>
"""
    (out / "index.html").write_text(html, encoding="utf-8", newline="\n")
    (out / "search-index.json").write_text(
        json.dumps({"documents": entries, "backlinks": backlinks}, indent=2),
        encoding="utf-8",
        newline="\n",
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=None)
    parser.add_argument(
        "--out",
        type=Path,
        default=None,
        help="Output directory (default: <root>/docs-site)",
    )
    args = parser.parse_args(argv)
    root = (args.root or Path(__file__).resolve().parent.parent).resolve()
    out = (args.out or (root / "docs-site")).resolve()
    out.mkdir(parents=True, exist_ok=True)

    docs = load_docs_from_tree(root)
    entries = build_search_index(docs)
    backlinks: dict[str, list[str]] = defaultdict(list)
    for doc in docs:
        for target in collect_links(doc.body, doc.path):
            backlinks[target].append(doc.path)

    write_summary(docs, out)
    write_html(entries, dict(backlinks), out)

    # Small README for the build dir
    (out / "README.md").write_text(
        textwrap_readme(),
        encoding="utf-8",
        newline="\n",
    )
    print(f"generated docs site → {out}")
    print(f"  documents indexed: {len(entries)}")
    print(f"  SUMMARY.md, index.html, search-index.json")
    return 0


def textwrap_readme() -> str:
    return """# Generated documentation site

This directory is produced by `python scripts/gen-docs-site.py`.

It is a **view** of repository Markdown (and links toward Rustdoc), not a second
source of truth. Prefer editing files under `docs/` and regenerating.

For mdBook: copy or point `src` at `docs/` and use the generated `SUMMARY.md` as a starting outline.
"""


if __name__ == "__main__":
    sys.exit(main())
