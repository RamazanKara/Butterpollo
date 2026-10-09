#!/usr/bin/env python3
"""Build the GitHub Pages site into _site/.

The landing page is site/index.html. The documentation pages are rendered
from the repository's own Markdown, so they always match docs/ and rust/.
Links between rendered pages stay on the site; links to source files go to
GitHub. The download link and version come from README.md's "Download"
link, which every release updates.

    python3 -m pip install -r site/requirements.txt
    python3 site/build.py          # writes _site/
"""

import html
import posixpath
import re
import shutil
import sys
from pathlib import Path

from markdown_it import MarkdownIt
from mdit_py_plugins.anchors import anchors_plugin

ROOT = Path(__file__).resolve().parent.parent
SITE = ROOT / "site"
OUT = ROOT / "_site"
REPO = "https://github.com/RamazanKara/Rubylight"

# Markdown rendered as pages, repository paths.
PAGES = sorted(
    [p.relative_to(ROOT).as_posix() for p in (ROOT / "docs").glob("*.md")]
    + [
        "rust/PARITY.md",
        "rust/PERFORMANCE.md",
        "rust/PERFORMANCE_WORK.md",
        "rust/README.md",
        "rust/RELEASE_NOTES.md",
        "rust/THIRD_PARTY.md",
    ]
)
# Files copied as they are when a page or the landing page links them.
MEDIA = {".png", ".jpg", ".jpeg", ".gif", ".svg", ".webp", ".mp4"}
STATIC = ["style.css", "favicon.svg", "poster.jpg"]
LANDING_MEDIA = ["docs/media/demo.mp4"]
# The documentation pages' top bar.
NAV = [
    ("Docs", "docs/index.html"),
    ("Performance", "docs/performance.html"),
    ("Compatibility", "rust/PARITY.html"),
    ("Release notes", "rust/RELEASE_NOTES.html"),
]


def out_path(src):
    """Site path of a rendered repository Markdown file; README.md is the landing page."""
    head, name = posixpath.split(src)
    name = "index.html" if name == "README.md" else name[:-3] + ".html"
    return posixpath.join(head, name)


def rel(target, page):
    """Relative link from one site path to another."""
    return posixpath.relpath(target, posixpath.dirname(page) or ".")


copied = set()


def copy_media(src):
    if src not in copied:
        dest = OUT / src
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / src, dest)
        copied.add(src)


broken = []


def rewrite(url, src):
    """Point a link in repository Markdown at the site or at GitHub."""
    if not url or url.startswith(("#", "//")) or re.match(r"[a-zA-Z][a-zA-Z0-9+.-]*:", url):
        return url
    path, hash_, fragment = url.partition("#")
    target = posixpath.normpath(posixpath.join(posixpath.dirname(src), path))
    page = out_path(src)
    if target == "README.md" or target in PAGES:
        return rel(out_path(target), page) + hash_ + fragment
    if not (ROOT / target).exists() or target.startswith(".."):
        broken.append(f"{src}: {url}")
        return url
    if (ROOT / target).is_file() and posixpath.splitext(target)[1].lower() in MEDIA:
        copy_media(target)
        return rel(target, page) + hash_ + fragment
    kind = "tree" if (ROOT / target).is_dir() else "blob"
    return f"{REPO}/{kind}/main/{target}" + hash_ + fragment


def markdown():
    md = MarkdownIt("commonmark", {"html": True, "typographer": False})
    md.enable(["table", "strikethrough"])
    # GitHub's heading ids, so links like performance.md#next-to-vibepollo-20 work.
    md.use(anchors_plugin, min_level=1, max_level=6, permalink=True, permalinkSymbol="#")
    md.add_render_rule("table_open", lambda *a: '<div class="table-wrap"><table>\n')
    md.add_render_rule("table_close", lambda *a: "</table></div>\n")
    return md


def render(md, src):
    tokens = md.parse((ROOT / src).read_text(encoding="utf-8"))
    title, toc = None, []
    for i, token in enumerate(tokens):
        if token.type == "heading_open":
            text = "".join(
                c.content for c in tokens[i + 1].children if c.type in ("text", "code_inline")
            ).strip()
            if token.tag == "h1" and title is None:
                title = text
            elif token.tag == "h2":
                toc.append((token.attrGet("id"), text))
        if token.type == "inline":
            for child in token.children or []:
                if child.type == "link_open":
                    child.attrSet("href", rewrite(child.attrGet("href"), src))
                elif child.type == "image":
                    child.attrSet("src", rewrite(child.attrGet("src"), src))
    mark_long_columns(tokens)
    body = md.renderer.render(tokens, md.options, {})
    return title or src, toc, body


def mark_long_columns(tokens):
    """Give columns of long prose a minimum width, so phones scroll the table
    instead of squeezing that column to a few words a line."""
    cells = []
    for i, token in enumerate(tokens):
        if token.type == "table_open":
            cells = []
        elif token.type == "tr_open":
            column = 0
        elif token.type in ("th_open", "td_open"):
            cells.append((column, token, len(tokens[i + 1].content)))
            column += 1
        elif token.type == "table_close":
            longest = {}
            for column, _, length in cells:
                longest[column] = max(longest.get(column, 0), length)
            for column, cell, _ in cells:
                if longest[column] > 160:
                    cell.attrSet("class", "long")


def fill(template, values):
    return re.sub(r"\{\{(\w+)\}\}", lambda m: values[m.group(1)], template)


def release():
    readme = (ROOT / "README.md").read_text(encoding="utf-8")
    match = re.search(r"\[Download ([^\]]+)\]\((" + re.escape(REPO) + r"/releases/tag/([^)\s]+))\)", readme)
    if not match:
        sys.exit("README.md has no [Download …](…/releases/tag/…) link")
    label, url, version = match.groups()
    return {"download_label": label, "download_url": url, "version": version}


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir()
    values = release()
    for name in STATIC:
        shutil.copy2(SITE / name, OUT / name)
    for src in LANDING_MEDIA:
        copy_media(src)
    (OUT / "index.html").write_text(
        fill((SITE / "index.html").read_text(encoding="utf-8"), values), encoding="utf-8"
    )

    md = markdown()
    template = (SITE / "page.html").read_text(encoding="utf-8")
    for src in PAGES:
        title, toc, body = render(md, src)
        page = out_path(src)
        root = "../" * page.count("/")
        nav = "".join(
            f'<a href="{root}{href}"{" aria-current=page" if href == page else ""}>{label}</a>'
            for label, href in NAV
        )
        contents = ""
        if len(toc) >= 5:
            items = "".join(f'<li><a href="#{id_}">{html.escape(text)}</a></li>' for id_, text in toc)
            contents = f'<details class="toc"><summary>On this page</summary><ol>{items}</ol></details>'
        dest = OUT / page
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_text(
            fill(
                template,
                {
                    **values,
                    "title": html.escape(title),
                    "root": root,
                    "nav": nav,
                    "toc": contents,
                    "content": body,
                    "source": f"{REPO}/blob/main/{src}",
                },
            ),
            encoding="utf-8",
        )

    if broken:
        print("Links to missing files:\n  " + "\n  ".join(broken), file=sys.stderr)
        sys.exit(1)
    print(f"Built {len(PAGES)} pages and the landing page into {OUT.relative_to(ROOT)}/")


if __name__ == "__main__":
    main()
