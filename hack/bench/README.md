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
| `chart.py` | draws `docs/img/compare-light.svg` and `compare-dark.svg` from a results file |

The Python scripts need Python 3.8+ and nothing outside the standard
library.

Only Skiff has been measured so far. Rows for other programs are added
only when they were measured by this procedure on the same machine, and
they are identified by what kind of program they are, not by product
name; [docs/benchmarks.md](../../docs/benchmarks.md#comparison-with-similar-software)
says why.

Linux is the tested platform. macOS is supported for `app` through `ps`,
`footprint` and `lsof`, and that path is **untested**: treat its output as
unverified until someone has checked it against Activity Monitor.

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

The generic mode has been run here only against Skiff itself (to test
the script), not against any other program.

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
  they are part of what runs on the machine.
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
| `conditions.os`, `.notes` | optional; not printed |
| `rows[].label` | required, unique. `Skiff <version>` for Skiff. For anything else a neutral description of the kind of program, with a letter when two are of one kind: "Electron-based client A", "terminal client in a terminal emulator". **Not a product name, and no version that identifies one** |
| `rows[].category` | required: `native` (draws its own interface), `electron` (bundles a browser engine), `webview` (uses the system's web view), `browser` (a web interface opened in a browser, which is measured too), `terminal` (a terminal interface, measured with its terminal emulator), `other` (say what in `notes`) |
| `rows[].subject` | `true` on exactly one row, the one the figure is about (Skiff): drawn first and in the accent colour |
| `rows[].version`, `.launch`, `.ready_by` | optional records of what was measured and how the pod list was judged ready; not printed |
| `rows[].notes` | optional; printed under the figure, so keep it neutral too |
| `rows[].metrics` | any of the five keys below; a missing one is drawn as "not measured" |

A metric is a number, or an object with `value` and optionally `min`,
`max`, `n` (how many runs `value` is the median of) and `note` (printed
under the figure).

| Metric | Unit | From |
| --- | --- | --- |
| `memory_pss_bytes` | bytes | `result.loaded.total.pss` of a `.steady.json`: PSS of the whole process tree 5 s after the pod list was loaded |
| `processes` | count | `result.loaded.total.processes` of the same file |
| `idle_cpu_percent` | % of one core | `result.idle.percent_of_one_core` of the same file |
| `window_visible_s` | seconds | `window_s` of each run in a `.startup-window.json` |
| `download_bytes` | bytes | size of the one file a user downloads to install the app on this platform |

Skiff's row in `results/2026-11-23-linux-x86_64-kvm.json` is the median
of the five steady runs and ten window launches behind
`docs/benchmarks.md`; its download is the `.tar.gz`.

Add or replace a row from the files of a run (this copies the numbers,
so nothing is retyped):

```sh
hack/bench/results.py add hack/bench/results/mine.json \
  --label "Electron-based client A" --category electron \
  --steady target/bench/mine/other-1.2.3.steady.json \
  --window target/bench/mine/other-1.2.3.startup-window.json \
  --download ~/Downloads/the-file-you-downloaded
```

`--steady` may be repeated; `--subject` marks Skiff's row. For a new
machine, copy an existing results file, rewrite its `conditions` from
`machine.txt` and set `"rows": []` first. The label used for the run, the
command line and all paths stay in the raw files under `target/`, which
are not checked in.

## Drawing the figures

```sh
hack/bench/chart.py --check hack/bench/results/mine.json    # only validate the file
hack/bench/chart.py hack/bench/results/mine.json --out /tmp/figures
hack/bench/chart.py hack/bench/results/2026-11-23-linux-x86_64-kvm.json --copy site/img
```

The last line is how the checked-in figures were made: without `--out`
the SVGs go to `docs/img`, and `--copy` writes them to a second
directory. Two files are written, `compare-light.svg` for light pages
and `compare-dark.svg` for dark ones, in the colours of
`crates/skiff/src/theme.rs`. A file with one row gives a strip of that
row's five numbers. A file with two or more rows gives one panel of
horizontal bars for each metric, each panel on its own scale from zero,
the subject's bar in the accent colour. The conditions and every note
are printed under the figure. To look at a figure as a PNG:
`rsvg-convert -b white compare-light.svg -o compare-light.png`.

## What is measured, and how

| Number | How (Linux) | macOS (untested) |
| --- | --- | --- |
| Process tree | the launched process, all its descendants, and anything else in its session (`/proc/*/stat`) | descendants (`ps -axo pid,ppid`) |
| Memory, PSS | sum of `Pss` in `/proc/<pid>/smaps_rollup` over the tree: private pages in full, shared pages divided among the processes sharing them | sum of `phys_footprint` from `footprint -p` |
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
