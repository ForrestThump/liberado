use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const BODY_INDEX_CAP: usize = 80_000;

#[derive(Clone, Debug)]
struct Document {
    path: String,
    meta: Map<String, Value>,
    body: String,
}

pub fn run(args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut root = None;
    let mut out = None;
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(args.next().ok_or("--root needs a path")?)),
            "--out" => out = Some(PathBuf::from(args.next().ok_or("--out needs a path")?)),
            _ => return Err(format!("unknown docs site argument: {arg}").into()),
        }
    }

    let root = root.unwrap_or(crate::crate_map_cmd::repository_root()?);
    let root = root.canonicalize()?;
    let out = out.unwrap_or_else(|| root.join("docs-site"));
    let out = if out.is_absolute() {
        out
    } else {
        root.join(out)
    };
    out.mkdir_all()?;

    let docs = load_docs(&root)?;
    let entries = build_search_index(&docs);
    let backlinks = build_backlinks(&docs)?;
    let mirrored = mirror_pages(&root, &docs, &out)?;
    write_summary(&docs, &out)?;
    write_html(&entries, &backlinks, &out)?;
    fs::write(out.join("README.md"), readme())?;

    println!("generated docs site -> {}", out.display());
    println!("  documents indexed: {}", entries.len());
    println!("  pages mirrored: {mirrored}");
    println!("  SUMMARY.md, index.html, search-index.json, pages/");
    Ok(())
}

trait CreateDirAll {
    fn mkdir_all(&self) -> std::io::Result<()>;
}

impl CreateDirAll for Path {
    fn mkdir_all(&self) -> std::io::Result<()> {
        fs::create_dir_all(self)
    }
}

fn load_docs(root: &Path) -> Result<Vec<Document>, Box<dyn std::error::Error>> {
    let mut paths = Vec::new();
    collect_markdown(&root.join("docs"), &mut paths)?;
    paths.sort();
    let mut docs = Vec::new();
    for path in paths {
        if path.file_name().and_then(|name| name.to_str())
            == Some("session-profiles-next-actions.md")
        {
            continue;
        }
        let text = String::from_utf8_lossy(&fs::read(&path)?).replace("\r\n", "\n");
        let (meta, body) = split_frontmatter(&text);
        docs.push(Document {
            path: path
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/"),
            meta,
            body,
        });
    }
    Ok(docs)
}

fn collect_markdown(dir: &Path, paths: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_markdown(&path, paths)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            paths.push(path);
        }
    }
    Ok(())
}

fn split_frontmatter(text: &str) -> (Map<String, Value>, String) {
    if !text.starts_with("---\n") {
        return (Map::new(), text.to_owned());
    }
    let Some(end) = text[4..].find("\n---\n") else {
        return (Map::new(), text.to_owned());
    };
    let yaml = &text[4..4 + end];
    let body = text[4 + end + 5..].to_owned();
    let meta = serde_yaml::from_str::<serde_yaml::Value>(yaml)
        .ok()
        .and_then(|value| serde_json::to_value(value).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    (meta, body)
}

fn build_search_index(docs: &[Document]) -> Vec<Value> {
    docs.iter()
        .map(|doc| {
            let title = doc
                .body
                .lines()
                .find_map(|line| line.strip_prefix("# "))
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| {
                    Path::new(&doc.path)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                });
            let body: String = doc.body.chars().take(BODY_INDEX_CAP).collect();
            json!({
                "path": doc.path,
                "href": page_href(&doc.path),
                "title": title,
                "kind": meta_string(&doc.meta, "kind"),
                "status": meta_string(&doc.meta, "status"),
                "authority": meta_string(&doc.meta, "authority"),
                "domain": meta_string(&doc.meta, "domain"),
                "canonical_for": meta_string(&doc.meta, "canonical_for"),
                "supersedes": doc.meta.get("supersedes").cloned().unwrap_or_else(|| json!([])),
                "superseded_by": meta_string(&doc.meta, "superseded_by"),
                "body": body,
            })
        })
        .collect()
}

fn meta_string(meta: &Map<String, Value>, key: &str) -> String {
    meta.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn page_href(path: &str) -> String {
    format!("pages/{path}")
}

fn build_backlinks(
    docs: &[Document],
) -> Result<BTreeMap<String, Vec<String>>, Box<dyn std::error::Error>> {
    let link_re = Regex::new(r"\[[^\]\r\n]*\]\(([^)]+)\)")?;
    let mut backlinks = BTreeMap::<String, Vec<String>>::new();
    for doc in docs {
        for capture in link_re.captures_iter(&doc.body) {
            let target = capture[1].split('#').next().unwrap_or("").trim();
            if target.is_empty()
                || target.ends_with('/')
                || ["http://", "https://", "mailto:", "//"]
                    .iter()
                    .any(|prefix| target.starts_with(prefix))
            {
                continue;
            }
            if let Some(resolved) = normalize_relative_path(&doc.path, target) {
                backlinks
                    .entry(resolved)
                    .or_default()
                    .push(doc.path.clone());
            }
        }
    }
    Ok(backlinks)
}

fn normalize_relative_path(from: &str, target: &str) -> Option<String> {
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    let target = target.replace('\\', "/");
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

fn mirror_pages(
    root: &Path,
    docs: &[Document],
    out: &Path,
) -> Result<usize, Box<dyn std::error::Error>> {
    let pages = out.join("pages");
    if pages.exists() {
        fs::remove_dir_all(&pages)?;
    }
    fs::create_dir_all(&pages)?;
    let mut count = 0;
    for doc in docs {
        let source = root.join(&doc.path);
        if !source.is_file() {
            continue;
        }
        let destination = pages.join(&doc.path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
        count += 1;
    }
    Ok(count)
}

fn write_summary(docs: &[Document], out: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut by_kind = BTreeMap::<String, Vec<&Document>>::new();
    for doc in docs {
        by_kind
            .entry(meta_string(&doc.meta, "kind"))
            .or_default()
            .push(doc);
    }
    let mut output = [
        "# Summary",
        "",
        "Generated outline for the docs-site catalog. mdBook users: point `src` at the",
        "repository `docs/` tree and use this file as a starting outline, or set",
        "`src` to this output directory after generation.",
        "",
        "[Home](README.md)",
        "",
    ]
    .join("\n");
    output.push('\n');
    for (kind, mut documents) in by_kind {
        documents.sort_by_key(|doc| doc.path.as_str());
        output.push_str(&format!("## {kind}\n\n"));
        for doc in documents {
            let title = Path::new(&doc.path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&doc.path);
            output.push_str(&format!("- [{title}]({})\n", page_href(&doc.path)));
        }
        output.push('\n');
    }
    fs::write(out.join("SUMMARY.md"), output)?;
    Ok(())
}

fn write_html(
    entries: &[Value],
    backlinks: &BTreeMap<String, Vec<String>>,
    out: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let docs = serde_json::to_string(entries)?;
    let backlinks_json = serde_json::to_string(backlinks)?;
    let html = r##"<!DOCTYPE html>
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
  <h2>Status index</h2><ul id="by-status"></ul>
  <h2>Domain index</h2><ul id="by-domain"></ul>
  <script>
    const DOCS = __DOCS__;
    const BACKLINKS = __BACKLINKS__;
    const results = document.getElementById('results');
    const count = document.getElementById('count');
    function escapeHtml(s) {{ return String(s).replace(/[&<>"']/g, c => ({{'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}})[c]); }}
    function render(list) {{
      results.innerHTML = ''; count.textContent = list.length + ' document(s)';
      for (const d of list) {{
        const li = document.createElement('li'); const bl = (BACKLINKS[d.path] || []).slice(0, 5).join(', ');
        const href = d.href || ('pages/' + d.path);
        li.innerHTML = '<strong><a href="' + escapeHtml(href) + '">' + escapeHtml(d.title) + '</a></strong> ' +
          '<span class="meta">[' + escapeHtml(d.status || '?') + ' · ' + escapeHtml(d.kind || '?') + ' · ' + escapeHtml(d.authority || '?') +
          (d.domain ? ' · ' + escapeHtml(d.domain) : '') + ']</span><br/><a class="path" href="' + escapeHtml(href) + '">' + escapeHtml(d.path) + '</a>' +
          (d.superseded_by ? '<br/><span class="meta">superseded by: ' + escapeHtml(String(d.superseded_by)) + '</span>' : '') +
          (bl ? '<br/><span class="meta">backlinks: ' + escapeHtml(bl) + '</span>' : '');
        results.appendChild(li);
      }}
    }}
    function filter(q) {{ q = q.trim().toLowerCase(); if (!q) return DOCS; return DOCS.filter(d =>
      (d.title + ' ' + d.path + ' ' + d.status + ' ' + d.kind + ' ' + d.domain + ' ' + d.authority + ' ' + d.canonical_for + ' ' + (d.body || '')).toLowerCase().includes(q)); }}
    document.getElementById('q').addEventListener('input', e => render(filter(e.target.value))); render(DOCS);
    function fillGroup(id, key) {{ const ul = document.getElementById(id); const groups = {{}};
      for (const d of DOCS) {{ const k = d[key] || '(none)'; (groups[k] = groups[k] || []).push(d); }}
      for (const k of Object.keys(groups).sort()) {{ const li = document.createElement('li'); li.innerHTML = '<strong>' + escapeHtml(k) + '</strong> (' + groups[k].length + '): ' +
        groups[k].slice(0, 8).map(d => '<a class="path" href="' + escapeHtml(d.href || ('pages/' + d.path)) + '">' + escapeHtml(d.path) + '</a>').join(', ') + (groups[k].length > 8 ? '...' : ''); ul.appendChild(li); }}
    }}
    fillGroup('by-status', 'status'); fillGroup('by-domain', 'domain');
  </script>
</body>
</html>
"##
    .to_string()
    .replace("__DOCS__", &docs)
    .replace("__BACKLINKS__", &backlinks_json);
    fs::write(out.join("index.html"), html)?;
    fs::write(
        out.join("search-index.json"),
        serde_json::to_string_pretty(&json!({"documents": entries, "backlinks": backlinks}))?,
    )?;
    Ok(())
}

fn readme() -> &'static str {
    "# Generated documentation catalog\n\nProduced by `liberado docs site`.\n\n## What this is\n\n- A **searchable catalog** of repository Markdown (title, path, metadata, **full body text**).\n- Mirrored pages under `pages/docs/...` so result links open real files in this tree.\n- `SUMMARY.md` for an optional mdBook-style outline of the same catalog.\n\n## What this is not\n\n- Not a second editable wiki. Edit files under `docs/` in git and regenerate.\n- Not a hosted product site. `docs/book.toml` documents optional mdBook use of the `docs/` tree; this generator's `SUMMARY.md` lives **here** in the output directory.\n\n## Regenerate\n\n```text\nliberado docs site\nliberado docs site --out path/to/out\n```\n"
}

#[cfg(test)]
mod tests {
    use super::{normalize_relative_path, split_frontmatter};

    #[test]
    fn parses_frontmatter_and_body() {
        let (meta, body) = split_frontmatter("---\nkind: plan\nopen_items: true\n---\n# Hello\n");
        assert_eq!(meta["kind"], "plan");
        assert_eq!(meta["open_items"], true);
        assert_eq!(body, "# Hello\n");
    }

    #[test]
    fn resolves_relative_links() {
        let docs = ["do", "cs"].concat();
        let nested = format!("{docs}/a/b.md");
        let document = format!("{docs}/a.md");
        assert_eq!(
            normalize_relative_path(&nested, "../c.md"),
            Some(format!("{docs}/c.md"))
        );
        assert_eq!(
            normalize_relative_path(&document, "../README.md"),
            Some("README.md".into())
        );
    }
}
