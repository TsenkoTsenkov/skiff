#!/usr/bin/env python3
"""Builds the documentation pages of the site: docs/*.md -> site/docs/*.html.

    python3 hack/site-build.py            # writes site/docs/
    python3 hack/site-build.py --out DIR  # writes DIR instead

Standard library only. The pages take their header and footer from
site/index.html (the parts between the "shared:" comments) and their look
from site/style.css, so the landing page and the documentation cannot
drift apart. The output is not committed: .github/workflows/pages.yml runs
this before publishing, and .gitignore lists site/docs/.

The Markdown converter below covers what the files in docs/ use and no
more: headings, paragraphs, bullet, numbered and task lists (nested),
tables, fenced code, block quotes, rules, raw HTML blocks such as
<picture>, and inline code, links, images, bold and italic. It is not a
general Markdown implementation.

The build fails, naming the file, when a link or an anchor between pages
leads nowhere, or when prose contains an HTML tag that GitHub would drop
(for example <cluster> written outside backticks).
"""

import argparse
import filecmp
import html
import os
import re
import shutil
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DOCS = os.path.join(ROOT, "docs")
SITE = os.path.join(ROOT, "site")
REPO_URL = "https://github.com/TsenkoTsenkov/skiff"

# The sidebar: groups of pages in reading order, with the short name each
# has there. A page in docs/ that is not listed is added to the last group
# under its own title, so a new file is never left out.
SIDEBAR = [
    ("Using Skiff", [
        ("README", "Overview"),
        ("install", "Install and update"),
        ("clusters", "Clusters and sign-in"),
        ("configuration", "Configuration"),
        ("keyboard", "Keyboard"),
        ("observability", "Metrics and alerts"),
        ("troubleshooting", "Troubleshooting"),
    ]),
    ("How it behaves", [
        ("security", "Security"),
        ("benchmarks", "Benchmarks"),
    ]),
    ("Project", [
        ("development", "Development"),
        ("releasing", "Releasing"),
    ]),
]

# Inline HTML that Markdown prose may use. Anything else that looks like a
# tag is an error: GitHub's renderer would silently remove it.
INLINE_TAGS = {"br", "kbd", "b", "i", "em", "strong", "code", "sub", "sup", "span"}
# Tags that start a raw HTML block, passed through until the next blank line.
BLOCK_TAGS = ("picture", "details", "summary", "p", "div", "img", "source", "table", "video", "figure", "h1", "h2",
              "h3", "a")


class BuildError(Exception):
    pass


def esc(text):
    return html.escape(text, quote=False)


def attr(text):
    return html.escape(text, quote=True)


def slugify(text, used):
    """The id GitHub gives a heading, so that links work in both places."""
    slug = re.sub(r"[^\w\- ]", "", text.strip().lower()).replace(" ", "-")
    base, n = slug, 0
    while slug in used:
        n += 1
        slug = f"{base}-{n}"
    used.add(slug)
    return slug


def strip_tags(markup):
    return html.unescape(re.sub(r"<[^>]+>", "", markup))


class Page:
    """One Markdown file and what converting it produced."""

    def __init__(self, name, builder):
        self.name = name                       # "install", "README"
        self.source = f"docs/{name}.md"
        self.out = "index.html" if name == "README" else f"{name}.html"
        self.builder = builder
        self.ids = set()
        self.headings = []                     # (level, id, html)
        self.links = []                        # (target page name or None for this page, fragment)
        self.title = name
        self.description = ""
        self.body = ""

    def fail(self, message):
        raise BuildError(f"{self.source}: {message}")

    # ---- Addresses -------------------------------------------------------

    def url(self, target, image=False):
        """Where a link or image written in the Markdown leads on the site."""
        if re.match(r"^[a-zA-Z][a-zA-Z0-9+.-]*:|^//", target):
            return target
        if target.startswith("#"):
            self.links.append((None, target[1:]))
            return target
        path, _, fragment = target.partition("#")
        rel = os.path.normpath(os.path.join("docs", path)).replace(os.sep, "/")
        full = os.path.join(ROOT, rel)
        if rel.startswith("../") or not os.path.exists(full):
            self.fail(f"link to {target!r}: there is no such file")
        m = re.fullmatch(r"docs/([^/]+)\.md", rel)
        if m and m.group(1) in self.builder.pages and not image:
            other = self.builder.pages[m.group(1)]
            if fragment:
                self.links.append((other.name, fragment))
            return other.out + ("#" + fragment if fragment else "")
        if os.path.isfile(full) and (image or rel.startswith("docs/")) and not rel.endswith(".md"):
            return self.builder.asset(rel)
        # Anything else in the repository is shown by GitHub.
        kind = "tree" if os.path.isdir(full) else "blob"
        return f"{REPO_URL}/{kind}/main/{rel}" + ("#" + fragment if fragment else "")

    def html_urls(self, markup):
        """Rewrites src, srcset, poster and href inside raw HTML."""
        def one(m):
            return f'{m.group(1)}="{attr(self.url(html.unescape(m.group(2)), image=m.group(1) != "href"))}"'
        return re.sub(r'\b(src|srcset|poster|href)="([^"]*)"', one, markup)

    # ---- Inline ----------------------------------------------------------

    def inline(self, text):
        stash = []

        def keep(markup):
            stash.append(markup)
            return f"\x00{len(stash) - 1}\x00"

        def code(m):
            body = m.group(2).replace("\n", " ")
            if body.startswith(" ") and body.endswith(" ") and body.strip():
                body = body[1:-1]
            return keep(f"<code>{esc(body)}</code>")

        text = re.sub(r"(?<!`)(`+)(?!`)(.+?)(?<!`)\1(?!`)", code, text, flags=re.S)
        text = re.sub(r"\\([\\`*_{}\[\]()#+\-.!|<>~])", lambda m: keep(esc(m.group(1))), text)

        def tag(m):
            name = m.group(2).lower()
            if name in INLINE_TAGS:
                return keep(m.group(0))
            self.fail(f"{m.group(0)!r} in prose would be read as an HTML tag and dropped by GitHub; "
                      "put it in backticks")

        text = re.sub(r"<(/?)([a-zA-Z][\w-]*)((?:\s[^<>]*)?)/?>", tag, text)
        text = re.sub(r"<(https?://[^\s<>]+)>", lambda m: keep(f'<a href="{attr(m.group(1))}">{esc(m.group(1))}</a>'),
                      text)
        text = re.sub(r"&amp;(#?\w+;)", r"&\1", esc(text))

        def image(m):
            title = f' title="{attr(m.group(3))}"' if m.group(3) else ""
            src = self.url(html.unescape(m.group(2)), image=True)
            return keep(f'<img src="{attr(src)}" alt="{attr(html.unescape(self.plain(m.group(1), stash)))}"{title} '
                        'loading="lazy">')

        target = r'\(\s*([^()\s]*(?:\([^()\s]*\)[^()\s]*)*)(?:\s+"(.*?)")?\s*\)'
        text = re.sub(r"!\[((?:[^\[\]]|\[[^\[\]]*\])*)\]" + target, image, text)

        def link(m):
            href = self.url(html.unescape(m.group(2)))
            label = keep(m.group(1)) if re.match(r"https?://", m.group(1)) else m.group(1)
            return keep(f'<a href="{attr(href)}">') + label + keep("</a>")

        text = re.sub(r"\[((?:[^\[\]]|\[[^\[\]]*\])*)\]" + target, link, text)
        text = re.sub(r"(?<![\w/\x00])(https?://[^\s<>\x00]*[^\s<>\x00.,;:!?)\]'\"])",
                      lambda m: keep(f'<a href="{m.group(1)}">') + m.group(1) + keep("</a>"), text)
        text = re.sub(r"\*\*(?=\S)(.+?)(?<=\S)\*\*", r"<strong>\1</strong>", text, flags=re.S)
        text = re.sub(r"(?<![\w*])\*(?=[^\s*])(.+?)(?<=[^\s*])\*(?![\w*])", r"<em>\1</em>", text, flags=re.S)
        text = re.sub(r"(?<![\w_])_(?=[^\s_])(.+?)(?<=[^\s_])_(?![\w_])", r"<em>\1</em>", text, flags=re.S)
        text = re.sub(r"(?: {2,}|\\)\n", "<br>\n", text)
        while "\x00" in text:
            text = re.sub(r"\x00(\d+)\x00", lambda m: stash[int(m.group(1))], text)
        return text

    @staticmethod
    def plain(text, stash):
        """Alt text: the stashed pieces put back, without their markup."""
        while "\x00" in text:
            text = re.sub(r"\x00(\d+)\x00", lambda m: re.sub(r"<[^>]+>", "", stash[int(m.group(1))]), text)
        return text

    # ---- Blocks ----------------------------------------------------------

    FENCE = re.compile(r"^( {0,3})(`{3,}|~{3,})\s*([\w+.#-]*)\s*$")
    HEADING = re.compile(r"^ {0,3}(#{1,6})\s+(.*?)(?:\s+#+)?\s*$")
    RULE = re.compile(r"^ {0,3}([-*_])(?: *\1){2,} *$")
    ITEM = re.compile(r"^( *)([-*+]|\d{1,9}[.)])( +)(\S.*)?$")
    QUOTE = re.compile(r"^ {0,3}> ?(.*)$")
    DELIM = re.compile(r"^ {0,3}\|?\s*:?-+:?\s*(?:\|\s*:?-+:?\s*)*\|?\s*$")
    HTML = re.compile(r"^ {0,3}<(?:/?(?:%s)\b|!--)" % "|".join(BLOCK_TAGS))

    def starts_block(self, lines, i):
        line = lines[i]
        return bool(self.FENCE.match(line) or self.HEADING.match(line) or self.RULE.match(line)
                    or self.QUOTE.match(line) or self.HTML.match(line) or self.starts_table(lines, i)
                    or (self.ITEM.match(line) and len(self.ITEM.match(line).group(1)) < 4
                        and self.ITEM.match(line).group(2) in ("-", "*", "+", "1.", "1)")))

    def starts_table(self, lines, i):
        return "|" in lines[i] and i + 1 < len(lines) and "-" in lines[i + 1] and bool(self.DELIM.match(lines[i + 1]))

    def blocks(self, lines, tight=False):
        out = []
        i = 0
        while i < len(lines):
            line = lines[i]
            if not line.strip():
                i += 1
                continue
            m = self.FENCE.match(line)
            if m:
                indent, fence, lang = len(m.group(1)), m.group(2), m.group(3)
                body = []
                i += 1
                closing = re.compile(r"^ {0,3}%s{%d,}\s*$" % (re.escape(fence[0]), len(fence)))
                while i < len(lines) and not closing.match(lines[i]):
                    body.append(re.sub(r"^ {0,%d}" % indent, "", lines[i]))
                    i += 1
                if i >= len(lines):
                    self.fail("a code fence is never closed")
                i += 1
                cls = f' class="language-{attr(lang)}"' if lang else ""
                out.append(f"<pre><code{cls}>{esc(chr(10).join(body))}\n</code></pre>")
                continue
            m = self.HEADING.match(line)
            if m:
                level = len(m.group(1))
                inner = self.inline(m.group(2))
                ident = slugify(strip_tags(inner), self.ids)
                self.headings.append((level, ident, inner))
                if level == 1:
                    out.append(f'<h1 id="{ident}">{inner}</h1>')
                else:
                    out.append(f'<h{level} id="{ident}">{inner}<a class="anchor" href="#{ident}" '
                               f'aria-label="Link to this section">#</a></h{level}>')
                i += 1
                continue
            if self.RULE.match(line):
                out.append("<hr>")
                i += 1
                continue
            if self.HTML.match(line):
                raw = []
                while i < len(lines) and lines[i].strip():
                    raw.append(lines[i])
                    i += 1
                out.append(self.html_urls("\n".join(raw)))
                continue
            if self.QUOTE.match(line):
                quoted = []
                while i < len(lines) and lines[i].strip() and (self.QUOTE.match(lines[i]) or not self.starts_block(lines, i)):
                    q = self.QUOTE.match(lines[i])
                    quoted.append(q.group(1) if q else lines[i])
                    i += 1
                note = re.match(r"^\[!(NOTE|TIP|IMPORTANT|WARNING|CAUTION)\]\s*$", quoted[0]) if quoted else None
                if note:
                    kind = note.group(1).lower()
                    out.append(f'<div class="admonition {kind}"><p class="admonition-title">{kind.capitalize()}</p>\n'
                               f"{self.blocks(quoted[1:])}\n</div>")
                else:
                    out.append(f"<blockquote>\n{self.blocks(quoted)}\n</blockquote>")
                continue
            if self.starts_table(lines, i):
                i = self.table(lines, i, out)
                continue
            m = self.ITEM.match(line)
            if m and len(m.group(1)) < 4:
                i = self.list(lines, i, out)
                continue
            para = [line.strip()]
            i += 1
            while i < len(lines) and lines[i].strip() and not self.starts_block(lines, i):
                para.append(lines[i].strip())
                i += 1
            inner = self.inline("\n".join(para))
            out.append(inner if tight else f"<p>{inner}</p>")
        return "\n".join(out)

    @staticmethod
    def cells(row):
        row = row.strip()
        if row.startswith("|"):
            row = row[1:]
        if row.endswith("|") and not row.endswith("\\|"):
            row = row[:-1]
        return [c.strip().replace("\\|", "|") for c in re.split(r"(?<!\\)\|", row)]

    def table(self, lines, i, out):
        head = self.cells(lines[i])
        align = []
        for cell in self.cells(lines[i + 1]):
            left, right = cell.startswith(":"), cell.endswith(":")
            align.append("center" if left and right else "right" if right else "left" if left else "")
        if len(align) != len(head):
            self.fail(f"table header {lines[i].strip()!r} and the line under it have different numbers of columns")
        i += 2

        def row(cells, tag):
            cells = (cells + [""] * len(head))[:len(head)]
            tds = []
            for cell, a in zip(cells, align):
                style = f' style="text-align:{a}"' if a else ""
                # A long key or path in a cell may wrap after a dot or a slash, so that a wide table still fits the column.
                html = re.sub(r"<code>(.*?)</code>",
                              lambda m: "<code>" + re.sub(r"(?<=[A-Za-z0-9>;])([./\\])(?=[A-Za-z0-9<&$%~*])", r"\1<wbr>", m.group(1)) + "</code>",
                              self.inline(cell))
                tds.append(f"<{tag}{style}>{html}</{tag}>")
            return "<tr>" + "".join(tds) + "</tr>"

        body = []
        while i < len(lines) and lines[i].strip() and "|" in lines[i]:
            body.append(row(self.cells(lines[i]), "td"))
            i += 1
        out.append('<div class="table-scroll"><table>\n<thead>' + row(head, "th") + "</thead>\n<tbody>\n"
                   + "\n".join(body) + "\n</tbody>\n</table></div>")
        return i

    def list(self, lines, i, out):
        first = self.ITEM.match(lines[i])
        base = len(first.group(1))
        ordered = first.group(2)[0].isdigit()
        items = []
        loose = False
        while i < len(lines):
            m = self.ITEM.match(lines[i])
            if not m or len(m.group(1)) != base or m.group(2)[0].isdigit() != ordered or self.RULE.match(lines[i]):
                break
            gap = len(m.group(3)) if len(m.group(3)) <= 4 and m.group(4) else 1
            content = base + len(m.group(2)) + gap
            item = [lines[i][content:] if m.group(4) else ""]
            i += 1
            while i < len(lines):
                line = lines[i]
                if not line.strip():
                    j = i
                    while j < len(lines) and not lines[j].strip():
                        j += 1
                    if j < len(lines) and len(lines[j]) - len(lines[j].lstrip(" ")) >= content:
                        item.extend([""] * (j - i))
                        i = j
                        continue
                    break
                indent = len(line) - len(line.lstrip(" "))
                if indent >= content:
                    item.append(line[content:])
                elif self.ITEM.match(line) or self.starts_block(lines, i) or not item[-1].strip():
                    break
                else:
                    item.append(line.strip())      # a paragraph line that was not indented
                i += 1
            if "" in item:
                loose = True
            items.append(item)
            j = i
            while j < len(lines) and not lines[j].strip():
                j += 1
            nxt = self.ITEM.match(lines[j]) if j < len(lines) else None
            if j > i and nxt and len(nxt.group(1)) == base and nxt.group(2)[0].isdigit() == ordered:
                loose = True
                i = j
        tasks = any(re.match(r"\[[ xX]\] ", item[0]) for item in items)
        rendered = []
        for item in items:
            box = ""
            t = re.match(r"\[([ xX])\] +", item[0])
            if t:
                checked = " checked" if t.group(1) != " " else ""
                box = f'<input type="checkbox" disabled{checked}> '
                item[0] = item[0][t.end():]
            inner = self.blocks(item, tight=not loose)
            if box and inner.startswith("<p>"):
                inner = "<p>" + box + inner[3:]
            else:
                inner = box + inner
            rendered.append(f'<li{" class=task" if t else ""}>{inner}</li>')
        tag = "ol" if ordered else "ul"
        start = ""
        if ordered and not first.group(2).startswith("1"):
            start = f' start="{int(first.group(2)[:-1])}"'
        out.append(f'<{tag}{start}{" class=tasks" if tasks else ""}>\n' + "\n".join(rendered) + f"\n</{tag}>")
        return i

    def convert(self):
        with open(os.path.join(ROOT, self.source), encoding="utf-8") as f:
            text = f.read().replace("\r\n", "\n").expandtabs(4)
        self.body = self.blocks(text.split("\n"))
        h1 = [h for h in self.headings if h[0] == 1]
        if len(h1) != 1:
            self.fail(f"expected exactly one top-level heading, found {len(h1)}")
        self.title = strip_tags(h1[0][2])
        first = re.search(r"<p>(.*?)</p>", self.body, flags=re.S)
        words = " ".join(strip_tags(first.group(1)).split()) if first else ""
        self.description = words if len(words) <= 200 else words[:197].rsplit(" ", 1)[0] + "…"


class Builder:
    def __init__(self, out):
        self.out = out
        names = sorted(f[:-3] for f in os.listdir(DOCS) if f.endswith(".md"))
        self.pages = {n: Page(n, self) for n in names}
        self.assets = {}                       # repository path -> address relative to a docs page

    def asset(self, rel):
        """An image or other file a page shows. One that the landing page
        already has under site/img/ is used from there; others are copied."""
        if rel not in self.assets:
            name = os.path.basename(rel)
            shared = os.path.join(SITE, "img", name)
            if os.path.isfile(shared) and filecmp.cmp(shared, os.path.join(ROOT, rel), shallow=False):
                self.assets[rel] = f"../img/{name}"
            else:
                sub = rel[len("docs/"):] if rel.startswith("docs/") else "assets/" + name
                dest = os.path.join(self.out, sub)
                os.makedirs(os.path.dirname(dest), exist_ok=True)
                shutil.copyfile(os.path.join(ROOT, rel), dest)
                self.assets[rel] = sub
        return self.assets[rel]

    def shared(self, name):
        """The landing page's header or footer, with its addresses turned
        into ones that work from site/docs/."""
        with open(os.path.join(SITE, "index.html"), encoding="utf-8") as f:
            page = f.read()
        m = re.search(r"<!-- shared:%s -->\n(.*?)<!-- /shared:%s -->" % (name, name), page, flags=re.S)
        if not m:
            raise BuildError(f"site/index.html has no <!-- shared:{name} --> … <!-- /shared:{name} --> part")

        def href(m):
            target = m.group(2)
            if re.match(r"^[a-zA-Z][a-zA-Z0-9+.-]*:|^//", target):
                return m.group(0)
            if target.startswith("#"):
                target = "../index.html" + (target if len(target) > 1 else "")
            elif target.startswith("docs/"):
                target = target[len("docs/"):]
            else:
                target = "../" + target
            return f'{m.group(1)}="{target}"'

        part = re.sub(r'\b(href|src)="([^"]*)"', href, m.group(1))
        return part.replace('<a href="index.html">Docs</a>', '<a href="index.html" aria-current="page">Docs</a>')

    def sidebar(self):
        groups = [(title, [(n, label) for n, label in entries if n in self.pages]) for title, entries in SIDEBAR]
        listed = {n for _, entries in groups for n, _ in entries}
        groups[-1][1].extend((n, self.pages[n].title) for n in self.pages if n not in listed)
        return [g for g in groups if g[1]]

    def nav(self, groups, current):
        out = []
        for title, entries in groups:
            out.append(f'<p class="docs-group">{esc(title)}</p>\n<ul>')
            for name, label in entries:
                here = ' aria-current="page"' if name == current.name else ""
                out.append(f'<li><a href="{self.pages[name].out}"{here}>{esc(label)}</a></li>')
            out.append("</ul>")
        return "\n".join(out)

    def toc(self, page):
        entries = [h for h in page.headings if h[0] in (2, 3)]
        if len(entries) < 2:
            return ""
        out = ["<ul>"]
        for level, ident, inner in entries:
            text = esc(strip_tags(inner))
            out.append(f'<li class="l{level}"><a href="#{ident}">{text}</a></li>')
        out.append("</ul>")
        return "\n".join(out)

    def build(self):
        if os.path.isdir(self.out) and os.listdir(self.out):
            # Only ever empty a folder this script filled.
            if not os.path.isfile(os.path.join(self.out, "index.html")):
                raise BuildError(f"{self.out} exists and does not look like a previous build; not touching it")
            shutil.rmtree(self.out)
        os.makedirs(self.out, exist_ok=True)
        for page in self.pages.values():
            page.convert()
        problems = []
        for page in self.pages.values():
            for name, fragment in page.links:
                other = self.pages[name] if name else page
                if fragment not in other.ids:
                    problems.append(f"{page.source}: no heading for #{fragment} in {other.source}")
        if problems:
            raise BuildError("\n".join(problems))

        header, footer = self.shared("header"), self.shared("footer")
        groups = self.sidebar()
        order = [n for _, entries in groups for n, _ in entries]
        labels = {n: label for _, entries in groups for n, label in entries}
        for page in self.pages.values():
            at = order.index(page.name)
            steps = []
            if at > 0:
                prev = self.pages[order[at - 1]]
                steps.append(f'<a class="prev" href="{prev.out}"><span>Previous</span>{esc(labels[prev.name])}</a>')
            if at + 1 < len(order):
                nxt = self.pages[order[at + 1]]
                steps.append(f'<a class="next" href="{nxt.out}"><span>Next</span>{esc(labels[nxt.name])}</a>')
            nav = self.nav(groups, page)
            toc = self.toc(page)
            title = "Skiff documentation" if page.name == "README" else f"{page.title} · Skiff documentation"
            doc = f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{esc(title)}</title>
<meta name="description" content="{attr(page.description)}">
<meta name="color-scheme" content="dark light">
<meta name="theme-color" content="#0E1014" media="(prefers-color-scheme: dark)">
<meta name="theme-color" content="#FFFFFF" media="(prefers-color-scheme: light)">
<link rel="icon" type="image/png" sizes="64x64" href="../img/icon-64.png">
<link rel="stylesheet" href="../style.css">
</head>
<body class="docs-page">

<a class="skip" href="#main">Skip to content</a>

{header}
<div class="wrap docs-layout">
  <nav class="docs-side" aria-label="Documentation pages">
{nav}
  </nav>
  <main id="main" class="doc">
    <details class="docs-menu">
      <summary><span>Documentation</span> {esc(labels[page.name])}</summary>
      <nav aria-label="Documentation pages">
{nav}
      </nav>
    </details>
{page.body}
    <nav class="doc-steps" aria-label="Previous and next page">
      {' '.join(steps)}
    </nav>
    <p class="doc-edit"><a href="{REPO_URL}/blob/main/{page.source}">This page on GitHub</a></p>
  </main>
  <aside class="docs-toc" aria-label="On this page">
    {'<p class="docs-group">On this page</p>' + chr(10) + toc if toc else ''}
  </aside>
</div>

{footer}
<script src="../site.js"></script>
</body>
</html>
"""
            with open(os.path.join(self.out, page.out), "w", encoding="utf-8") as f:
                f.write(doc)
        return len(self.pages)


def main():
    parser = argparse.ArgumentParser(description="Build site/docs/ from docs/*.md.")
    parser.add_argument("--out", default=os.path.join(SITE, "docs"), help="folder to write (default: site/docs)")
    args = parser.parse_args()
    try:
        count = Builder(os.path.abspath(args.out)).build()
    except BuildError as e:
        print(f"site-build: {e}", file=sys.stderr)
        return 1
    print(f"site-build: {count} pages written to {os.path.relpath(args.out)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
