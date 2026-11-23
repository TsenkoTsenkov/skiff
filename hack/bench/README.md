# Benchmark scripts

`run.sh` measures what a desktop Kubernetes client costs the machine it runs
on. It is the procedure behind [docs/benchmarks.md](../../docs/benchmarks.md),
and it is not specific to Skiff: it measures any app from the outside, given
the command that starts it. `measure.py` does the measuring (Python 3.8+,
standard library only); `run.sh` sets up the display and calls it.

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

## Measuring another app

The same steady-state procedure, for anything that has a launch command:

```sh
export KUBECONFIG=$PWD/hack/kubeconfig     # or add the cluster in the app
hack/bench/run.sh app --label "Name 1.2.3" --installed /path/to/the/installed/app -- <launch command>
```

1. The script starts the app and tells you to open **the list of all pods**
   (all namespaces) of the test cluster.
2. **Press Enter in the terminal when the pod list is showing**, fully
   loaded. Then leave the app alone: do not move the mouse over it.
3. Five seconds later the script records memory, processes and threads,
   then watches CPU for 60 idle seconds, takes a screenshot, and closes
   the app.
4. It prints a table row. Paste it into the comparison table in
   `docs/benchmarks.md`, with the machine description from
   `<out>/machine.txt`.

Examples (launch commands depend on how the app was installed; none of
these has been run by us):

```sh
hack/bench/run.sh app --label "Headlamp X.Y.Z" --installed /opt/Headlamp -- /opt/Headlamp/headlamp
hack/bench/run.sh app --label "Freelens X.Y.Z" --installed ~/Applications/Freelens.AppImage -- ~/Applications/Freelens.AppImage
hack/bench/run.sh app --label "k9s X.Y.Z in xterm" --installed "$(command -v k9s)" -- xterm -e k9s --context scale -A -c pods
```

Rules that keep a comparison fair:

- Same machine, same cluster, same day, and say which. Write the exact
  version of each app in the label.
- Same state: the list of all pods, loaded, nothing else opened. If an app
  cannot show all namespaces at once, say so next to its row.
- Do not pass `--xvfb` on a machine with a real GPU and compare with a row
  that was measured on one without. Xvfb has no GPU, so a software
  rasterizer draws; that costs memory and CPU, differently for each app.
- An app measured under a terminal emulator (k9s) includes the terminal
  emulator: it is part of what runs on the machine.
- A row is one run. For a number you want to publish, run it three times
  and report the middle one, or all three.
- `--ready-after SEC` replaces the Enter key with a fixed wait. Use it only
  when the screenshot proves the list was loaded by then.
- Start times of other apps are not measured by this script, because an
  app that does not say when its list is complete cannot be timed without
  a person or screen-scraping, and neither is exact. Skiff's own start
  times come from lines it prints (`SKIFF_TRACE_STARTUP=1`).

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
