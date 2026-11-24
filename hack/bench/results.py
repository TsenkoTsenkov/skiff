#!/usr/bin/env python3
"""Adds or replaces a row in a results file from what run.sh wrote.

    hack/bench/results.py add hack/bench/results/<file>.json \\
        --label "Electron-based client A" --category electron \\
        --steady <out>/<slug>.steady.json [--steady ...] \\
        [--window <out>/<slug>.startup-window.json] \\
        [--download /path/to/the/downloaded/file] [--notes "..."]

Copies numbers only: memory (PSS) and process count 5 s after the pod list
was loaded, idle CPU, the time to a visible window, and the size of the
downloaded file. With several --steady files the value is their median
and the smallest and largest are kept. The label the run was made under,
the command line and the paths stay in the raw files; the row gets the
neutral label given here. See README.md, "The results file".
"""

import argparse
import json
import os
import statistics
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import chart  # noqa: E402  (the schema lives there)


def stat(values):
    if len(values) == 1:
        return {"value": values[0], "n": 1}
    return {"value": statistics.median(values), "min": min(values), "max": max(values), "n": len(values)}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="mode", required=True)
    p = sub.add_parser("add")
    p.add_argument("results")
    p.add_argument("--label", required=True, help="what the row is called in the figure; for anything but Skiff a neutral description, not a product name")
    p.add_argument("--category", required=True, choices=sorted(chart.CATEGORIES))
    p.add_argument("--subject", action="store_true", help="this is the row the figure is about (Skiff)")
    p.add_argument("--steady", action="append", required=True, metavar="JSON", help="a .steady.json of a run; repeat for several runs")
    p.add_argument("--window", metavar="JSON", help="the .startup-window.json of run.sh --window-runs")
    p.add_argument("--download", metavar="PATH", help="the file a user downloads to install the app")
    p.add_argument("--download-note", help="what that file is, if it needs saying (keep it neutral)")
    p.add_argument("--notes", help="anything a reader must know about this row")
    args = ap.parse_args()

    with open(args.results) as f:
        doc = json.load(f)
    steady = []
    for path in args.steady:
        with open(path) as f:
            r = json.load(f)["result"]
        if r.get("mode") != "steady":
            sys.exit(f"{path} is not a steady-state result")
        if r["loaded"]["total"].get("pss") is None:
            sys.exit(f"{path} has no PSS (not Linux, or smaps_rollup was not readable): this row cannot be compared")
        steady.append(r)
    metrics = {
        "memory_pss_bytes": stat([r["loaded"]["total"]["pss"] for r in steady]),
        "processes": stat([r["loaded"]["total"]["processes"] for r in steady]),
        "idle_cpu_percent": stat([r["idle"]["percent_of_one_core"] for r in steady]),
    }
    if args.window:
        with open(args.window) as f:
            runs = [r.get("window_s") for r in json.load(f)["result"]["runs"]]
        if None in runs:
            sys.exit(f"{args.window}: a run never showed a window; measure again")
        metrics["window_visible_s"] = stat(runs)
    if args.download:
        metrics["download_bytes"] = {"value": os.stat(args.download).st_size}
        if args.download_note:
            metrics["download_bytes"]["note"] = args.download_note
    row = {"label": args.label, "category": args.category}
    if args.subject:
        row["subject"] = True
    row["ready_by"] = ", ".join(sorted({r["ready_by"] for r in steady}))
    if args.notes:
        row["notes"] = args.notes
    row["metrics"] = metrics
    rows = [r for r in doc.get("rows", []) if r.get("label") != args.label]
    replaced = len(rows) != len(doc.get("rows", []))
    doc["rows"] = rows + [row]
    with open(args.results, "w") as f:
        json.dump(doc, f, indent=2, ensure_ascii=False)
        f.write("\n")
    chart.load(args.results)  # complains if the file is no longer valid
    print(f"{'replaced' if replaced else 'added'} row {args.label!r} in {args.results}")


if __name__ == "__main__":
    main()
