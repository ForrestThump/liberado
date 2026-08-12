#!/usr/bin/env python3
"""Generate a searchable documentation catalog from repository Markdown.

Outputs under `docs-site/` (gitignored) or `--out`:

  - index.html with client-side full-text search over title, path, metadata, and body
  - search-index.json (includes body text)
  - mirrored Markdown pages under pages/docs/... for navigable result links
  - SUMMARY.md for optional mdBook outline (lives in this output dir, not docs/)
  - README.md describing what this site is and is not

Does not introduce a second editable source of truth — all content is derived
from the repository tree.
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import sys
from collections import defaultdict
from html import escape
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from docs_meta import (  # noqa: E402
    configure_stdio,
    load_docs_from_tree,
    safe_print,
)

LINK_RE = re.compile(r"\[([^\]]*)\]\(([^)]+)\)")
BODY_INDEX_CAP = 80_000


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
        try:
            resolved = (base / t).as_posix()
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


def page_href(doc_path: str) -> str:
    """Relative URL from index.html to the mirrored page for doc_path."""
    return "pages/" + doc_path.replace("\\", "/")


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
        body = doc.body or ""
        if len(body) > BODY_INDEX_CAP:
            body = body[:BODY_INDEX_CAP]
        entries.append(
            {
                "path": doc.path,
                "href": page_href(doc.path),
                "title": title,
                "kind": meta.get("kind", ""),
                "status": meta.get("status", ""),
                "authority": meta.get("authority", ""),
                "domain": meta.get("domain", ""),
                "canonical_for": meta.get("canonical_for", ""),
                "supersedes": meta.get("supersedes", []),
                "superseded_by": meta.get("superseded_by", ""),
                "body": body,
            }
        )
    return entries


def write_summary(docs, out: Path) -> None:
    lines = [
        "# Summary",
        "",
        "Generated outline for the docs-site catalog. mdBook users: point `src` at the",
        "repository `docs/` tree and use this file as a starting outline, or set",
        "`src` to this output directory after generation.",
        "",
        "[Home](README.md)",
        "",
    ]
    by_kind: dict[str, list] = defaultdict(list)
    for doc in docs:
        kind = (doc.meta or {}).get("kind", "other")
        by_kind[kind].append(doc)
    for kind in sorted(by_kind):
        lines.append(f"## {kind}")
        lines.append("")
        for doc in sorted(by_kind[kind], key=lambda d: d.path):
            title = Path(doc.path).name
            lines.append(f"- [{title}]({page_href(doc.path)})")
        lines.append("")
    (out / "SUMMARY.md").write_text("\n".join(lines), encoding="utf-8", newline="\n")


def mirror_pages(root: Path, docs, out: Path) -> int:
    """Copy managed/source markdown into out/pages/ so result links resolve."""
    pages = out / "pages"
    if pages.exists():
        shutil.rmtree(pages)
    pages.mkdir(parents=True)
    n = 0
    for doc in docs:
        src = root / doc.path
        if not src.is_file():
            continue
        dest = pages / doc.path
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dest)
        n += 1
    return n


def write_html(entries: list[dict], backlinks: dict[str, list[str]], out: Path) -> None:
    # Drop huge bodies from the inline script? Keep them — full-text is the point.
    # Use compact JSON.
    index_json = json.dumps(entries, ensure_ascii=False, separators=(",", ":"))
    backlinks_json = json.dumps(backlinks, ensure_ascii=False, separators=(",", ":"))
    html = f"""<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8"/>
  <title>Liberado docs catalog</title>
  <style>
    body {{ font-family: system-ui, sans-serif; margin: 1.5rem; max-width: 960px; }}
    input {{ width: 100%; padding: 0.5rem; font-size: 1rem; }}
    .meta {{ color: #555; font-size: 0.85rem; }}
    li {{ margin: 0.4rem 0; }}
    code {{ font-size: 0.9em; }}
    a.path {{ font-family: ui-monospace, monospace; font-size: 0.9em; }}
  </style>
</head>
<body>
  <h1>Liberado documentation catalog (generated)</h1>
  <p>Full-text search over managed document <strong>titles, paths, metadata, and body text</strong>.
  Result paths link to mirrored Markdown under <code>pages/</code> in this output tree.
  Source of truth remains the git repository.</p>
  <p class="meta">Rustdoc (after <code>cargo doc --workspace --no-deps</code>):
  <code>target/doc/&lt;crate&gt;/index.html</code>.
  ADRs: <a href="pages/docs/decisions/README.md">docs/decisions/</a>.
  Authority: <a href="pages/docs/spec/reference/doc-authority.md">doc-authority.md</a>.</p>
  <input id="q" type="search" placeholder="Search title, path, status, domain, body..." autofocus/>
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
        const href = d.href || ('pages/' + d.path);
        li.innerHTML = '<strong><a href=\"' + escapeHtml(href) + '\">' +
          escapeHtml(d.title) + '</a></strong> ' +
          '<span class="meta">[' + escapeHtml(d.status || '?') + ' · ' +
          escapeHtml(d.kind || '?') + ' · ' + escapeHtml(d.authority || '?') +
          (d.domain ? ' · ' + escapeHtml(d.domain) : '') + ']</span><br/>' +
          '<a class="path" href=\"' + escapeHtml(href) + '\">' +
          escapeHtml(d.path) + '</a>' +
          (d.superseded_by ? '<br/><span class="meta">superseded by: ' +
            escapeHtml(String(d.superseded_by)) + '</span>' : '') +
          (bl ? '<br/><span class="meta">backlinks: ' + escapeHtml(bl) + '</span>' : '');
        results.appendChild(li);
      }}
    }}
    function escapeHtml(s) {{
      return String(s).replace(/[&<>\"']/g, c => ({{
        '&':'&amp;','<':'&lt;','>':'&gt;','\"':'&quot;',\"'\":'&#39;'
      }})[c]);
    }}
    function filter(q) {{
      q = q.trim().toLowerCase();
      if (!q) return DOCS;
      return DOCS.filter(d =>
        (d.title + ' ' + d.path + ' ' + d.status + ' ' + d.kind + ' ' +
         d.domain + ' ' + d.authority + ' ' + d.canonical_for + ' ' +
         (d.body || '')).toLowerCase().includes(q)
      );
    }}
    document.getElementById('q').addEventListener('input', e => render(filter(e.target.value)));
    render(DOCS);
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
          m[k].slice(0, 8).map(d => {{
            const href = d.href || ('pages/' + d.path);
            return '<a class="path" href=\"' + escapeHtml(href) + '\">' +
              escapeHtml(d.path) + '</a>';
          }}).join(', ') +
          (m[k].length > 8 ? '...' : '');
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
        json.dumps({"documents": entries, "backlinks": backlinks}, indent=2, ensure_ascii=False),
        encoding="utf-8",
        newline="\n",
    )


def textwrap_readme() -> str:
    return """# Generated documentation catalog

Produced by `python scripts/gen-docs-site.py`.

## What this is

- A **searchable catalog** of repository Markdown (title, path, metadata, **full body text**).
- Mirrored pages under `pages/docs/...` so result links open real files in this tree.
- `SUMMARY.md` for an optional mdBook-style outline of the same catalog.

## What this is not

- Not a second editable wiki. Edit files under `docs/` in git and regenerate.
- Not a hosted product site. `docs/book.toml` documents optional mdBook use of the
  `docs/` tree; this generator's `SUMMARY.md` lives **here** in the output directory.

## Regenerate

```text
python scripts/gen-docs-site.py
python scripts/gen-docs-site.py --out path/to/out
```
"""


def main(argv: list[str] | None = None) -> int:
    configure_stdio()
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

    n_pages = mirror_pages(root, docs, out)
    write_summary(docs, out)
    write_html(entries, dict(backlinks), out)
    (out / "README.md").write_text(textwrap_readme(), encoding="utf-8", newline="\n")

    safe_print(f"generated docs site -> {out}")
    safe_print(f"  documents indexed: {len(entries)}")
    safe_print(f"  pages mirrored: {n_pages}")
    safe_print("  SUMMARY.md, index.html, search-index.json, pages/")
    return 0


if __name__ == "__main__":
    sys.exit(main())
