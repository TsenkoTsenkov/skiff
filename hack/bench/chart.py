#!/usr/bin/env python3
"""Draws the figures of docs/benchmarks.md from a results file.

    hack/bench/chart.py hack/bench/results/<file>.json [--out docs/img] [--copy site/img]
    hack/bench/chart.py --check hack/bench/results/<file>.json

Writes compare-light.svg and compare-dark.svg (the same figure for light
and for dark pages) into each directory. With one row in the file the
figure is a strip of that row's numbers; with two or more it is one panel
of horizontal bars per metric, the row marked "subject" in the app's
accent colour and the others in a neutral one.

The results file is described in hack/bench/README.md ("The results
file"). Python 3.8+, standard library only. Nothing is measured here.
"""

import argparse
import json
import os
import sys
from xml.sax.saxutils import escape

SCHEMA = 1

# What a row may be called instead of a product name. Free text is allowed
# for the label; the category is one of these.
CATEGORIES = {
    "native": "native app (draws its own interface)",
    "electron": "Electron-based app (bundles a browser engine)",
    "webview": "app that uses the system's web view",
    "browser": "web interface opened in a browser (the browser is measured too)",
    "terminal": "terminal interface (the terminal emulator is measured too)",
    "other": "anything else; say what in the row's notes",
}

# key, panel title, what exactly it is (shown small), formatter name
METRICS = [
    ("memory_pss_bytes", "Memory", "whole process tree, PSS, pod list loaded", "mib"),
    ("processes", "Processes", "in the tree, pod list loaded", "count"),
    ("download_bytes", "Download", "size of the file you download", "mb"),
    ("window_visible_s", "Window visible", "after launch, warm start", "time"),
    ("idle_cpu_percent", "Idle CPU", "of one core, over 60 idle seconds", "percent"),
]
CONDITION_KEYS = ["date", "machine", "os", "rendering", "cluster", "state", "notes"]

# Colours of crates/skiff/src/theme.rs (Palette::light and Palette::dark).
THEMES = {
    "light": {
        "page": "#FFFFFF",  # only for --png previews; the SVG has no page fill
        "panel": "#F5F6F8",
        "border": "#DFE2E8",
        "text": "#1C2330",
        "muted": "#4F5968",
        "faint": "#626C7D",
        "accent": "#2367EA",
        "other": "#9AA3B2",  # Palette::outline
        "track": "#ECEEF2",  # Palette::hover
    },
    "dark": {
        "page": "#0E1014",
        "panel": "#13161B",
        "border": "#242933",
        "text": "#D5DAE3",
        "muted": "#9BA3B2",
        "faint": "#848D9D",
        "accent": "#5B9DFF",
        "other": "#5C6473",
        "track": "#1F242D",
    },
}
FONT = "Inter, -apple-system, 'Segoe UI', Helvetica, Arial, sans-serif"
WIDTH = 880


# ------------------------------------------------------------ the results


def fail(msg):
    sys.exit(f"chart.py: {msg}")


def load(path):
    """Reads and checks a results file. Returns (conditions, rows) with
    every metric as a dict that has at least "value", or None."""
    try:
        with open(path) as f:
            doc = json.load(f)
    except (OSError, ValueError) as e:
        fail(f"{path}: {e}")
    if doc.get("schema") != SCHEMA:
        fail(f"{path}: \"schema\" must be {SCHEMA}")
    cond = doc.get("conditions")
    if not isinstance(cond, dict):
        fail(f"{path}: \"conditions\" (an object) is required: one file is one machine, one cluster, one day")
    for key in ("date", "machine", "rendering", "cluster", "state"):
        if not isinstance(cond.get(key), str) or not cond[key].strip():
            fail(f"{path}: conditions.{key} is required (a sentence)")
    rows = doc.get("rows")
    if not isinstance(rows, list) or not rows:
        fail(f"{path}: \"rows\" needs at least one row")
    known = {m[0] for m in METRICS}
    seen = set()
    for i, row in enumerate(rows):
        where = f"{path}: rows[{i}]"
        label = row.get("label")
        if not isinstance(label, str) or not label.strip():
            fail(f"{where}: \"label\" is required")
        if label in seen:
            fail(f"{where}: label {label!r} is used twice")
        seen.add(label)
        if row.get("category") not in CATEGORIES:
            fail(f"{where}: \"category\" must be one of {', '.join(sorted(CATEGORIES))}")
        metrics = row.get("metrics")
        if not isinstance(metrics, dict):
            fail(f"{where}: \"metrics\" (an object) is required")
        for key in metrics:
            if key not in known:
                fail(f"{where}: unknown metric {key!r} (known: {', '.join(m[0] for m in METRICS)})")
        clean = {}
        for key in known:
            m = metrics.get(key)
            if m is None:
                clean[key] = None
                continue
            if isinstance(m, (int, float)) and not isinstance(m, bool):
                m = {"value": m}
            if not isinstance(m, dict) or not isinstance(m.get("value"), (int, float)) or m["value"] < 0:
                fail(f"{where}: metrics.{key} must be a number >= 0 or an object with \"value\"")
            for k in ("min", "max"):
                if k in m and not isinstance(m[k], (int, float)):
                    fail(f"{where}: metrics.{key}.{k} must be a number")
            if "min" in m and "max" in m and not m["min"] <= m["value"] <= m["max"]:
                fail(f"{where}: metrics.{key}: value is outside min - max")
            clean[key] = m
        row["metrics"] = clean
    subjects = [r for r in rows if r.get("subject")]
    if len(subjects) != 1:
        fail(f"{path}: exactly one row must have \"subject\": true (the row the figure is about)")
    # The subject first, the others in the order of the file.
    rows = subjects + [r for r in rows if not r.get("subject")]
    return cond, rows


# ------------------------------------------------------------- formatting


def fmt_num(v, unit):
    """(number, unit) as printed."""
    if unit == "mib":
        return f"{v / (1 << 20):,.0f}", "MiB"
    if unit == "mb":
        mb = v / 1e6
        return (f"{mb:,.0f}" if mb >= 100 else f"{mb:.1f}"), "MB"
    if unit == "count":
        return f"{v:,.0f}", ""
    if unit == "time":
        return (f"{v * 1000:.0f}", "ms") if v < 1 else (f"{v:.2f}", "s")
    if unit == "percent":
        return (f"{v:.2f}" if v < 1 else f"{v:.1f}" if v < 10 else f"{v:.0f}"), "%"
    raise ValueError(unit)


def fmt(v, unit):
    n, u = fmt_num(v, unit)
    return n + ("" if u in ("", "%") else " ") + u


def spread(m, unit):
    """"median of 5 (161 - 163 MiB)", as far as the row says."""
    parts = []
    if m.get("n", 1) > 1:
        parts.append(f"median of {m['n']}")
    elif m.get("n") == 1:
        parts.append("one run")
    if "min" in m and "max" in m and m["min"] != m["max"]:
        lo, u = fmt_num(m["min"], unit)
        hi, u2 = fmt_num(m["max"], unit)
        if u != u2:  # 950 ms - 1.20 s
            parts.append(f"{fmt(m['min'], unit)} - {fmt(m['max'], unit)}")
        else:
            parts.append(f"{lo} - {hi}{'' if u in ('', '%') else ' '}{u}")
    return ", ".join(parts)


def text_width(s, size, bold=False):
    """A rough width of s in Inter at this size; errs on the wide side."""
    w = 0.0
    for ch in s:
        if ch in "il.,:;'|!·":
            w += 0.30
        elif ch in " ftrjI-()":
            w += 0.38
        elif ch in "mwMW%":
            w += 0.90
        elif ch.isupper() or ch.isdigit():
            w += 0.68
        else:
            w += 0.57
    return w * size * (1.06 if bold else 1.0)


def wrap(s, size, width):
    lines, line = [], ""
    for word in s.split():
        trial = f"{line} {word}".strip()
        if line and text_width(trial, size) > width:
            lines.append(line)
            line = word
        else:
            line = trial
    if line:
        lines.append(line)
    return lines


# ------------------------------------------------------------------- SVG


class Svg:
    def __init__(self, theme):
        self.c = THEMES[theme]
        self.out = []

    def text(self, x, y, s, size=13, fill="text", weight=400, anchor="start", extra=""):
        a = f' text-anchor="{anchor}"' if anchor != "start" else ""
        w = f' font-weight="{weight}"' if weight != 400 else ""
        self.out.append(f'<text x="{x:g}" y="{y:g}" font-size="{size:g}" fill="{self.c[fill]}"{w}{a}{extra}>{escape(s)}</text>')

    def run(self, x, y, parts):
        """Several pieces of text on one line, each (text, size, fill,
        weight, gap before it): placed by the renderer, so nothing depends
        on guessing how wide a piece is."""
        spans = []
        for s, size, fill, weight, dx in parts:
            d = f' dx="{dx:g}"' if dx else ""
            spans.append(f'<tspan font-size="{size:g}" fill="{self.c[fill]}" font-weight="{weight}"{d}>{escape(s)}</tspan>')
        self.out.append(f'<text x="{x:g}" y="{y:g}">{"".join(spans)}</text>')

    def rect(self, x, y, w, h, fill, r=0, stroke=None, title=None):
        s = f' stroke="{self.c[stroke]}"' if stroke else ""
        rx = f' rx="{r:g}"' if r else ""
        tag = f'<rect x="{x:g}" y="{y:g}" width="{w:g}" height="{h:g}"{rx} fill="{self.c[fill]}"{s}'
        self.out.append(f"{tag}><title>{escape(title)}</title></rect>" if title else f"{tag}/>")

    def bar(self, x, y, w, h, fill, title):
        """A bar flat on the baseline (left) and rounded at the data end."""
        r = min(4, w / 2, h / 2)
        d = f"M{x:g},{y:g}h{w - r:g}a{r:g},{r:g} 0 0 1 {r:g},{r:g}v{h - 2 * r:g}a{r:g},{r:g} 0 0 1 {-r:g},{r:g}h{-(w - r):g}z"
        self.out.append(f'<path d="{d}" fill="{self.c[fill]}"><title>{escape(title)}</title></path>')

    def document(self, height, title, desc):
        head = (
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{height:g}" viewBox="0 0 {WIDTH} {height:g}" '
            f'role="img" aria-labelledby="t d" font-family="{FONT}">\n'
            f'<title id="t">{escape(title)}</title>\n<desc id="d">{escape(desc)}</desc>\n'
        )
        return head + "\n".join(self.out) + "\n</svg>\n"


def conditions_lines(cond):
    """The conditions as sentences for the foot of a figure."""
    first = f"Measured {cond['date']} on {cond['machine']}; {cond['rendering']}."
    second = f"{cond['cluster']}. {cond['state']}."
    return [first.replace("..", "."), second.replace("..", ".")]


def describe(cond, rows):
    """The figure in words, for <desc> (screen readers, and a text view)."""
    parts = []
    for row in rows:
        vals = [f"{title.lower()} {fmt(row['metrics'][key]['value'], unit)}" for key, title, _, unit in METRICS if row["metrics"][key]]
        parts.append(f"{row['label']}: " + ", ".join(vals) + ".")
    return " ".join(parts + conditions_lines(cond))


def foot(svg, cond, y, x=24, mentions=()):
    """Conditions (and any notes of the rows) in small print. Returns the y below."""
    lines = []
    for sentence in conditions_lines(cond) + list(mentions):
        lines += wrap(sentence, 11.5, WIDTH - 2 * x)
    for line in lines:
        svg.text(x, y, line, size=11.5, fill="faint")
        y += 17
    return y - 17


def single(cond, rows, theme):
    """One row: its numbers, side by side."""
    svg, row = Svg(theme), rows[0]
    pad, gap = 24, 0
    cols = [(k, t, s, u) for k, t, s, u in METRICS if row["metrics"][k]]
    notes = [f"{t}: {row['metrics'][k]['note']}." for k, t, _, _ in cols if row["metrics"][k].get("note")]
    foot_lines = sum(len(wrap(s, 11.5, WIDTH - 2 * pad)) for s in conditions_lines(cond) + notes)
    top, tile_h = 58, 104
    height = top + tile_h + 26 + foot_lines * 17 + 8
    svg.rect(0.5, 0.5, WIDTH - 1, height - 1, "panel", r=10, stroke="border")
    svg.run(pad, 36, [(row["label"], 17, "text", 600, 0), ("measured on the launched app", 13, "muted", 400, 10)])
    col_w = (WIDTH - 2 * pad - gap * (len(cols) - 1)) / len(cols)
    for i, (key, title, sub, unit) in enumerate(cols):
        m = row["metrics"][key]
        x = pad + i * (col_w + gap)
        if i:
            svg.out.append(f'<line x1="{x - 0.5:g}" y1="{top + 4}" x2="{x - 0.5:g}" y2="{top + tile_h - 8}" stroke="{svg.c["border"]}"/>')
            x += 18
        svg.rect(x, top + 6, 3, 11, "accent", r=1.5)
        svg.text(x + 10, top + 16, title, size=12.5, fill="muted", weight=600)
        num, u = fmt_num(m["value"], unit)
        svg.run(x, top + 56, [(num, 32, "text", 600, 0)] + ([(u, 15, "muted", 600, 3 if u == "%" else 6)] if u else []))
        for j, line in enumerate(wrap(sub, 11.5, col_w - 26)[:2]):
            svg.text(x, top + 76 + j * 15, line, size=11.5, fill="faint")
    foot(svg, cond, top + tile_h + 26, pad, notes)
    return svg.document(height, f"{row['label']}, measured", describe(cond, rows))


def multi(cond, rows, theme):
    """Several rows: one panel of horizontal bars per metric, each on its
    own scale from zero."""
    svg = Svg(theme)
    pad, bar_h, row_h, head_h, panel_gap = 24, 14, 26, 34, 14
    label_w = min(max(text_width(r["label"], 13, True) for r in rows) + 16, 260)
    value_w = 150  # room for the value and its spread after the longest bar
    x0 = pad + label_w
    plot_w = WIDTH - pad - x0 - value_w
    notes = []
    for row in rows:
        for key, title, _, _ in METRICS:
            m = row["metrics"][key]
            if m and m.get("note"):
                notes.append(f"{row['label']}, {title.lower()}: {m['note']}.")
        if row.get("notes"):
            notes.append(f"{row['label']}: {row['notes']}.")
    foot_lines = sum(len(wrap(s, 11.5, WIDTH - 2 * pad)) for s in conditions_lines(cond) + notes)
    panel_h = head_h + len(rows) * row_h + 6
    top = 58
    height = top + len(METRICS) * (panel_h + panel_gap) + 12 + foot_lines * 17 + 8
    svg.rect(0.5, 0.5, WIDTH - 1, height - 1, "panel", r=10, stroke="border")
    subject = rows[0]["label"]
    svg.text(pad, 36, f"{subject} and similar software", size=17, weight=600)
    svg.text(WIDTH - pad, 36, "same machine, same cluster, same day · each panel has its own scale", size=12, fill="muted", anchor="end")
    y = top
    for key, title, sub, unit in METRICS:
        svg.out.append(f'<line x1="{pad}" y1="{y - 0.5:g}" x2="{WIDTH - pad}" y2="{y - 0.5:g}" stroke="{svg.c["border"]}"/>')
        svg.run(pad, y + 22, [(title, 13, "text", 600, 0), (sub, 11.5, "faint", 400, 8)])
        top_value = max((r["metrics"][key]["value"] for r in rows if r["metrics"][key]), default=0)
        for i, row in enumerate(rows):
            m = row["metrics"][key]
            ry = y + head_h + i * row_h
            is_subject = i == 0
            svg.text(x0 - 12, ry + bar_h - 2.5, row["label"], size=13, fill="text" if is_subject else "muted", weight=600 if is_subject else 400, anchor="end")
            if not m:
                svg.text(x0, ry + bar_h - 2.5, "not measured", size=12, fill="faint", extra=' font-style="italic"')
                continue
            w = max(plot_w * m["value"] / top_value, 3) if top_value else 3
            tip = f"{row['label']} ({CATEGORIES[row['category']]}): {title.lower()} {fmt(m['value'], unit)}"
            more = spread(m, unit)
            svg.bar(x0, ry, w, bar_h, "accent" if is_subject else "other", tip + (f", {more}" if more else ""))
            value = fmt(m["value"], unit)
            svg.text(x0 + w + 8, ry + bar_h - 2.5, value, size=13, weight=600 if is_subject else 400)
        y += panel_h + panel_gap
    foot(svg, cond, y + 10, pad, notes)
    return svg.document(height, f"{subject} and similar software, measured", describe(cond, rows))


def render(cond, rows, theme):
    return (single if len(rows) == 1 else multi)(cond, rows, theme)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("results", help="a results file (hack/bench/results/*.json)")
    ap.add_argument("--out", default=None, help="directory for the SVGs (default docs/img of this repository)")
    ap.add_argument("--copy", action="append", default=[], metavar="DIR", help="also write them here (repeatable), e.g. site/img")
    ap.add_argument("--name", default="compare", help="file name stem (default compare: compare-light.svg, compare-dark.svg)")
    ap.add_argument("--check", action="store_true", help="only check the results file")
    args = ap.parse_args()
    cond, rows = load(args.results)
    if args.check:
        print(f"{args.results}: ok, {len(rows)} row(s): " + ", ".join(r["label"] for r in rows))
        return
    repo = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    for d in [args.out or os.path.join(repo, "docs", "img")] + args.copy:
        os.makedirs(d, exist_ok=True)
        for theme in THEMES:
            path = os.path.join(d, f"{args.name}-{theme}.svg")
            with open(path, "w") as f:
                f.write(render(cond, rows, theme))
            print(path)


if __name__ == "__main__":
    main()
