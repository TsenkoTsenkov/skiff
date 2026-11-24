# Benchmark scripts

`run.sh` measures what a desktop Kubernetes client costs the machine it runs
on. It is the procedure behind [docs/benchmarks.md](../../docs/benchmarks.md),
and it is not specific to Skiff: it measures any app from the outside, given
the command that starts it.

| File | What it does |
| --- | --- |
| `run.sh` | sets up the display and runs the procedure |
| `measure.py` | does the measuring |
| `results/*.json` | the numbers that are published, one file for each machine and day |
| `results.py` | copies numbers from a run into a results file |
| `chart.py` | draws the figures in `docs/img` from a results file: `compare-*.svg` (Linux, Skiff alone) and `compare-macos-*.svg` (macOS, three apps) |

The Python scripts need Python 3.8+ and nothing outside the standard
library.

Two sets of results are published. On Linux only Skiff has been measured
(`results/2026-11-23-linux-x86_64-kvm.json`). On macOS Skiff and two
similar programs were measured on one Mac
(`results/2026-11-24-macos-arm64-m1pro.json`). Rows for other programs
are added only when they were measured by this procedure on the same
machine, and they are identified by what kind of program they are, never
by product name or version;
[docs/benchmarks.md](../../docs/benchmarks.md#comparison-with-similar-software)
says why.

Linux is the tested platform. On macOS only `app` works, through `ps`,
`footprint` and `lsof` (no `--xvfb`, `--trace` or `--window-runs`). That
path has been run on one Mac, by the project's owner for the macOS
results, and no test covers it. Memory on macOS is the **physical
footprint**, the figure Activity Monitor shows. It is not PSS, which
macOS does not have, so a macOS memory figure is never put next to a
Linux one: the two have different metric keys and cannot share a results
file or a figure.

## What you need

- Linux: `python3`, `ss` (iproute2) or `lsof`. Optional: `Xvfb` (`--xvfb`),
  `strace` (`--trace`), `xdotool` (time to a visible window), ImageMagick
  `import` (the screenshot), `vulkaninfo` (records which GPU or software
  rasterizer draws).
- Run it as the user that runs the app, or as root: memory (PSS) is read
  from `/proc/<pid>/smaps_rollup`, which other users may not read.
- A cluster to look at. For numbers comparable with ours use the same one:
  `hack/cluster-up.sh` starts two local [kwok](https://kwok.sigs.k8s.io)
  clusters; context `scale` in `hack/kubeconfig` has 100 nodes and about
  5,400 pods.

## Measuring Skiff

```sh
cargo build --release -p skiff
hack/linux-package.sh --binary target/release/skiff --out target/dist   # optional: package sizes
hack/cluster-up.sh
hack/bench/run.sh skiff --runs 10 --xvfb --out target/bench/mine
```

This takes about eight minutes and writes JSON (plus the app's output, a
screenshot and the raw strace logs) into the `--out` directory. It stops
the test bed's churn loop while it runs and starts it again afterwards.

## Measuring similar software

The same steady-state procedure, for anything that has a launch command:

```sh
export KUBECONFIG=$PWD/hack/kubeconfig     # or add the cluster in the app
hack/bench/run.sh app --label "other 1.2.3" --window-runs 10 \
  --installed /path/to/the/installed/app --out target/bench/mine \
  -- <launch command>
```

1. With `--window-runs 10` the script launches the app once untimed (so
   that its files are in the page cache), then ten times, timing each
   launch until a window of the app is visible, and closes it each time.
   This needs `xdotool` and an X display.
2. It starts the app again and tells you to open **the list of all pods**
   (all namespaces) of the test cluster.
3. **Press Enter in the terminal when the pod list is showing**, fully
   loaded. Then leave the app alone: do not move the mouse over it.
4. Five seconds later the script records memory, processes and threads,
   then watches CPU for 60 idle seconds, takes a screenshot, and closes
   the app.
5. It prints a table row and writes `<slug>.steady.json`,
   `<slug>.size.json` and `<slug>.startup-window.json` into the `--out`
   directory, where `<slug>` is the label with spaces replaced by `-`.
   `machine.txt` in the same directory describes the machine.

The launch command depends on the kind of program and how it was
installed. Shapes that work (the paths are placeholders):

```sh
# a desktop app, installed in a directory or as a single file
hack/bench/run.sh app --label "other 1.2.3" --installed /opt/other -- /opt/other/other
# a terminal client: launch a terminal emulator that runs it
hack/bench/run.sh app --label "tui 1.2.3 in xterm" --installed "$(command -v tui)" -- xterm -e tui
```

On Linux the generic mode has been run only against Skiff itself (to
test the script). On macOS the project's owner ran it against Skiff and
two other programs for the 2026-11-24 results.

Rules that keep a comparison fair:

- Same machine, same cluster, same day, and say which. Write the exact
  version of each app in the `--label` of the run; it stays in the raw
  files.
- Same state: the list of all pods, loaded, nothing else opened. If an app
  cannot show all namespaces at once, say so in its row's notes.
- Do not pass `--xvfb` on a machine with a real GPU and compare with a row
  that was measured on one without. Xvfb has no GPU, so a software
  rasterizer draws; that costs memory and CPU, differently for each app.
- An app measured under a terminal emulator includes the terminal
  emulator, and a web interface includes the browser it is opened in:
  they are part of what runs on the machine. If a terminal client was
  measured without a terminal app around it, its row's notes must say
  so, because its numbers are then lower than what a user would see (the
  terminal row of the macOS results is such a row).
- Memory is PSS on Linux and the physical footprint on macOS. Never set
  one beside the other.
- One steady run gives one number. For a number you want to publish, run
  it three times and give all three files to `results.py`, which keeps
  the median and the range.
- `--ready-after SEC` replaces the Enter key with a fixed wait. Use it only
  when the screenshot proves the list was loaded by then.
- "Window visible" is the first moment any window of the app is on the
  display, whatever it shows; a splash screen counts. It says nothing
  about when the pod list is complete. That time is not measured for
  other apps, because an app that does not say when its list is complete
  cannot be timed without a person or screen-scraping, and neither is
  exact. Skiff's own start times come from lines it prints
  (`SKIFF_TRACE_STARTUP=1`).
- Do not copy a number from anywhere but this script's output.

## The results file

`results/<date>-<machine>.json` holds what is published. One file is one
set of conditions: one machine, one cluster, one day. Rows from different
machines never share a file, so the figure cannot mix them.

```json
{
  "schema": 1,
  "conditions": {
    "date": "2026-11-23",
    "machine": "a 4-vCPU cloud VM (KVM, Intel Xeon 2.10 GHz, 15.7 GiB RAM, no GPU)",
    "os": "Ubuntu 24.04.5, Linux 6.18, x86_64",
    "rendering": "software rendering (Mesa 25.2.8 lavapipe on Xvfb 1440x900), which inflates memory and CPU",
    "cluster": "Local kwok cluster, context scale: kube-apiserver 1.37.2, 100 nodes, 5,415 pods",
    "state": "The list of all pods in all namespaces is open and loaded; nothing else was opened",
    "caveats": ["a sentence printed under the figure", "another"],
    "notes": "free text"
  },
  "rows": [
    {
      "label": "Electron-based client A",
      "category": "electron",
      "ready_by": "manual (Enter pressed)",
      "notes": "free text, printed under the figure",
      "metrics": {
        "memory_pss_bytes": {"value": 0, "min": 0, "max": 0, "n": 3},
        "processes": {"value": 0, "n": 3},
        "download_bytes": {"value": 0, "note": "which file"},
        "window_visible_s": {"value": 0, "min": 0, "max": 0, "n": 10},
        "idle_cpu_percent": {"value": 0, "n": 3}
      }
    }
  ]
}
```

(The zeros are placeholders, not measurements.)

| Field | Meaning |
| --- | --- |
| `schema` | `1` |
| `conditions.date`, `.machine`, `.rendering`, `.cluster`, `.state` | required; phrases that are printed under the figure, so write them to be read. `.machine` and `.rendering` complete "Measured DATE on ...; ...", `.cluster` and `.state` are sentences without the full stop |
| `conditions.caveats` | optional list of sentences that are printed under the figure after the conditions: what a reader must know about every row (which kind of memory, that only medians were kept) |
| `conditions.os`, `.notes` | optional; not printed |
| `rows[].label` | required, unique. `Skiff <version>` for Skiff. For anything else a neutral description of the kind of program, with a letter when two are of one kind: "Electron-based client A", "terminal client in a terminal emulator". **Not a product name, and no version that identifies one** |
| `rows[].category` | required: `native` (draws its own interface), `electron` (bundles a browser engine), `webview` (uses the system's web view), `browser` (a web interface opened in a browser, which is measured too), `terminal` (a terminal interface; `notes` says what it ran in, a terminal emulator that is measured too or a bare wrapper), `other` (say what in `notes`) |
| `rows[].subject` | `true` on exactly one row, the one the figure is about (Skiff): drawn first and in the accent colour |
| `rows[].version`, `.launch`, `.ready_by`, `.other_hosts_first_minute` | optional records of what was measured, how the pod list was judged ready, and how many hosts other than the cluster the app connected to in its first minute; not printed. `version` is for Skiff's row only |
| `rows[].notes` | optional; printed under the figure, so keep it neutral too |
| `rows[].metrics` | any of the keys below, with one kind of memory per file. A metric that no row has gets no panel; one that only some rows have is drawn as "not measured" for the others |

A metric is a number, or an object with `value` and optionally `min`,
`max`, `n` (how many runs `value` is the median of), `note` (printed
under the figure) and `decimals` (print a percentage with exactly this
many decimals; for a value that was recorded already rounded, so that
0.1 is not printed as 0.10). Leave `min` and `max` out when they are not
known; never fill them in from the median.

| Metric | Unit | From |
| --- | --- | --- |
| `memory_pss_bytes` | bytes | Linux. `result.loaded.total.pss` of a `.steady.json`: PSS of the whole process tree 5 s after the pod list was loaded |
| `memory_footprint_bytes` | bytes | macOS. `result.loaded.total.footprint` of a `.steady.json`: the physical footprint (`phys_footprint` from `footprint -p`, the "Memory" column of Activity Monitor) summed over the process tree at the same moment |
| `processes` | count | `result.loaded.total.processes` of the same file |
| `idle_cpu_percent` | % of one core | `result.idle.percent_of_one_core` of the same file |
| `window_visible_s` | seconds | `window_s` of each run in a `.startup-window.json` |
| `download_bytes` | bytes | size of the one file a user downloads to install the app on this platform |

`memory_pss_bytes` and `memory_footprint_bytes` are different
quantities. `chart.py` refuses a file in which both occur, and
`results.py` refuses to add a row of one kind to a file that has rows of
the other, or to build one row from runs of both kinds.

Skiff's row in `results/2026-11-23-linux-x86_64-kvm.json` is the median
of the five steady runs and ten window launches behind
`docs/benchmarks.md`; its download is the `.tar.gz`.

`results/2026-11-24-macos-arm64-m1pro.json` has three rows, each the
median of three runs on an Apple M1 Pro. Its rows were written by hand
from the medians the owner reported, rounded as reported (memory to
1 MiB, idle CPU to 0.1%), because the raw files of those runs are on the
owner's machine. That is why its metrics have `n` but no `min` and
`max`. Download size and time to a visible window were not measured
there.

Add or replace a row from the files of a run (this copies the numbers,
so nothing is retyped):

```sh
hack/bench/results.py add hack/bench/results/mine.json \
  --label "Electron-based client A" --category electron \
  --steady target/bench/mine/other-1.2.3.steady.json \
  --window target/bench/mine/other-1.2.3.startup-window.json \
  --download ~/Downloads/the-file-you-downloaded
```

`--steady` may be repeated; `--subject` marks Skiff's row. The memory
metric is chosen by what the run holds: PSS from a Linux run, the
footprint from a macOS run. For a new
machine, copy an existing results file, rewrite its `conditions` from
`machine.txt` and set `"rows": []` first. The label used for the run, the
command line and all paths stay in the raw files under `target/`, which
are not checked in.

## Drawing the figures

```sh
hack/bench/chart.py --check hack/bench/results/mine.json    # only validate the file
hack/bench/chart.py hack/bench/results/mine.json --out /tmp/figures
hack/bench/chart.py hack/bench/results/2026-11-23-linux-x86_64-kvm.json --copy site/img
hack/bench/chart.py hack/bench/results/2026-11-24-macos-arm64-m1pro.json --name compare-macos --copy site/img
```

The last two lines are how the checked-in figures were made: without
`--out` the SVGs go to `docs/img`, and `--copy` writes them to a second
directory. Two files are written for each, `<name>-light.svg` for light
pages and `<name>-dark.svg` for dark ones (`--name`, default `compare`),
in the colours of `crates/skiff/src/theme.rs`. A file with one row gives
a strip of that row's numbers. A file with two or more rows gives one
panel of horizontal bars for each metric that a row has, each panel on
its own scale from zero, the subject's bar in the accent colour. The
conditions, the caveats and every note are printed under the figure. To
look at a figure as a PNG:
`rsvg-convert -b white compare-light.svg -o compare-light.png`.

## What is measured, and how

| Number | How (Linux) | macOS (run on one Mac, no test) |
| --- | --- | --- |
| Process tree | the launched process, all its descendants, and anything else in its session (`/proc/*/stat`) | descendants (`ps -axo pid,ppid`) |
| Memory, PSS | sum of `Pss` in `/proc/<pid>/smaps_rollup` over the tree: private pages in full, shared pages divided among the processes sharing them | no PSS. Instead the physical footprint: sum of `phys_footprint` from `footprint -p`. A different quantity, kept under its own key |
| Memory, RSS | sum of `Rss` over the tree; pages shared between the app's own processes are counted once per process, so for a multi-process app this overstates | sum of `ps -o rss` |
| Processes, threads | size of the tree; sum of `Threads` in `/proc/<pid>/status` | `ps`, `ps -M` |
| Idle CPU | user + system CPU time of the tree (`/proc/<pid>/stat`) used during the 60 s window, as a percentage of one core | `ps -o time` |
| Listening sockets | `ss -H -a -n -p -t` and `-u` filtered to the tree's pids, sampled five times a second from launch | `lsof -nP -i` |
| Remote addresses | the same snapshots: every peer address a socket of the tree had during the first 60 s | `lsof -nP -i` |
| Every connection (`--trace`) | a second run under `strace -f -e trace=connect,bind,listen,execve` | not available |
| Size on disk | file size, or the sum of file sizes under a directory (hard links once) | same |
| Launch timings (`startup`) | wall time from just before `fork`/`exec` to a line the app prints; with `--window`, to the first visible X11 window (`xdotool`, polled) | not available |

Known limits:

- The socket snapshots can miss a connection that opens and closes between
  two snapshots (0.2 s apart). `--trace` cannot miss one, but slows the
  app, so it is a separate run.
- A process that starts and ends inside the idle window takes its CPU time
  with it; the idle figure does not include it.
- An app that hands its work to a process started outside its own session
  (a system service, an already running instance) is undercounted. Close
  other instances first.
- The X server (or compositor) is not in the tree. An app that makes the
  display server do its drawing looks cheaper than it is.
- Under `strace` the first `exec` in the list is the app itself.

## Using measure.py directly

```sh
hack/bench/measure.py startup --label X --runs 7 --mark 'ready=REGEX' -- <command>
hack/bench/measure.py steady  --label X [--ready-regex RE | --ready-after SEC] -- <command>
hack/bench/measure.py trace   --label X --seconds 60 -- <command>
hack/bench/measure.py size    --label X PATH...
hack/bench/measure.py row     X.steady.json [X.size.json]
```

`hack/bench/measure.py --help` has the details.
