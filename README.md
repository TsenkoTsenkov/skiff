# Skiff

A fast, native Kubernetes cluster browser. Think Lens, without Electron: one
Rust binary, GPU-rendered UI, no account, no telemetry, no background
services. It talks to the API server directly, exactly like `kubectl` does,
using your existing kubeconfig.

Measured on a 4-core cloud VM against a local cluster with 100 nodes and
5,415 pods (release build; see [Development](#development) to reproduce):

| | |
| --- | --- |
| First frame after launch | 60–120 ms |
| All 5,415 pods listed and live | 0.4–0.5 s after launch |
| Frame cost with the pods table open | about 1 ms |
| Re-filter / re-sort 5,415 rows | 1–5 ms |
| Open an object's details | about 20 ms |
| Switch to another resource type (3,000 rows) | about 0.1 s |
| Memory held per pod row | under 1 KB (about 4 MB for 5,415 pods) |
| CPU while idle on that table | under 1% of one core |
| Binary | 35 MB, no runtime dependencies beyond a GPU driver |

That machine has no GPU, so rendering was done by a CPU rasterizer; process
memory there was about 165 MB, most of it the rasterizer. Numbers on real
hardware have not been measured yet.

## Why it is fast

- **Native, immediate-mode UI** (egui). There is no web view, no DOM and no
  JavaScript. A frame is about a millisecond of CPU, and the app repaints
  only when data changes or a relative time on screen would tick over.
- **Watches only what you look at.** A resource type is listed and watched
  when you open it and stops three minutes after you leave it. Lens-style
  "watch everything up front" is what makes large clusters painful.
- **Server-side tables.** Lists use the same Table API as `kubectl get`: the
  API server renders the columns (including CRD printer columns) and ships
  object metadata instead of full objects. Every resource type works with no
  client-side schema, secrets values never leave the server for a list, and a
  pod costs well under 1 KB of memory. Full objects are fetched only when you
  open one.
- **Streaming everything.** The first page of a list paints while the rest
  loads; watch events are applied in batches and published as immutable
  snapshots, so the UI thread never waits on the network or on a lock.
- **Small core.** `skiff-core` (the data layer) knows nothing about the UI
  and runs on two I/O threads.

What runs in a container is not trusted with the app's health: a shell's
output is taken in a few milliseconds per frame however fast it arrives,
queues between the app and the cluster are bounded in both directions, and
the terminal emulator runs behind a guard, so output it cannot digest costs
that one screen a reset rather than the whole app.

## Features

- Every resource type the cluster serves, including custom resources, with
  live updates, instant filter (`/`), sortable columns, a column chooser and
  columns that adapt to the available width
- Multi-cluster: switch kubeconfig contexts instantly; connections stay warm
- Namespace filter (one or many), remembered per cluster
- Detail panel with tailored summaries (pods and containers, workloads,
  services, ingresses, nodes, config maps, secrets with reveal/copy, PVCs,
  cron jobs, events, …), conditions, related pods, object events, and links
  between objects
- YAML view with syntax highlighting; edit and apply with conflict detection
  (the editor follows the object until you type, and tells you if it changed
  underneath your edits); create or update objects from a manifest
  (server-side apply, any number of documents)
- Pod and workload logs: follow, multi-pod merge, filter, timestamps, wrap,
  previous container, level colouring
- A shell in any container: a real terminal emulator in the dock (colours,
  full-screen programs such as `vi` and `top`, 10,000 lines of scrollback,
  mouse selection, copy and paste, resizing with the dock)
- Port forwarding for pods and services, with a list of active forwards
- Scale, rollout restart, cordon / uncordon, suspend / resume cron jobs,
  delete (optionally forced)
- CPU / memory columns and cluster usage from metrics-server (no Prometheus)
- Cluster overview: what is broken right now and recent warnings
- Command palette (`Ctrl/Cmd+K`): jump to any kind (kubectl short names
  work: `po`, `deploy`, `svc`, …), any loaded object by name, namespace,
  cluster or action
- Dark and light themes

## When the network misbehaves

- A watch that goes silent is treated as dead after 150 seconds and
  re-established; waking from sleep triggers an immediate refresh; a request
  that gets no answer is abandoned after 15 seconds and retried with backoff.
  The table says so (`reconnecting`, `polling`) instead of showing stale rows
  as live.
- If something between you and the API server cuts watches short, or your
  account may list but not watch, the table falls back to polling every ten
  seconds and tries watching again later.
- A proxy or load balancer that drops idle connections after 30 or 45
  seconds is not mistaken for that: a quiet table keeps its watch, and an
  occasional list confirms that nothing was missed. Only if that list shows
  changes the watch never delivered does the table switch to polling.
- Log following survives dropped connections and container restarts without
  losing or repeating lines, and follows a reconnect to new credentials.
- Closing a log tab, a shell or a port-forward closes its connection on the
  server too; the tests check this against the API server's own metrics.

## Build and run

Requirements: Rust 1.95 or newer.

```bash
cargo build --release
```

```bash
./target/release/skiff
```

Options: `--context <name>`, `--namespace <name>`, `--view <resource>` (e.g.
`--view pods`), `--light`. Clusters come from `$KUBECONFIG` or `~/.kube/config`; exec-based
auth plugins (EKS, GKE, …) and OIDC are supported.

Linux needs a Vulkan or OpenGL driver and `libxkbcommon` (on Debian/Ubuntu:
`libxkbcommon-x11-0`, plus `mesa-vulkan-drivers` if you have no GPU driver).
Windows needs nothing extra.

### macOS

A Mac build has to be made on a Mac (the linker needs Apple's own system
libraries). With the Xcode command line tools (`xcode-select --install`) and
Rust installed:

```bash
hack/macos-app.sh --install
```

builds the app and puts `Skiff.app` into `/Applications`; without `--install`
it stays in `target/release/`, and `--dmg` also makes a disk image.
`cargo build --release` and `./target/release/skiff` from a terminal work
as on Linux.

Without a Mac toolchain at hand, GitHub can build it: the workflow in
`.github/workflows/macos.yml` produces the disk image on an Apple Silicon
runner when a branch named `release/…` or a tag such as `v0.1.0` is pushed
(or when the workflow is started by hand); the image is an artifact of the
run and, for a tag, attached to a release. Such a downloaded copy is
not notarized by Apple, so the first launch has to be allowed once: System
Settings → Privacy & Security → Open Anyway, or

```bash
xattr -dr com.apple.quarantine /Applications/Skiff.app
```

Opened from the Finder or the Dock, the app asks your login shell for its
`PATH`, so that the credential helpers your kubeconfig calls (`aws`,
`gke-gcloud-auth-plugin`, `kubelogin`, …) are found.

The code compiles for Apple Silicon (checked by cross-compiling everything
up to the link step), but it has not been run on a Mac yet.

## Keyboard

| Key | Action |
| --- | --- |
| `Ctrl/Cmd+K` | Command palette |
| `/` | Focus the table filter |
| `↑` `↓` or `j` `k` | Move the selection |
| `PgUp` `PgDn` `Home` `End` | Move further |
| `Enter` | Open details |
| `Esc` | Close details / clear the filter |
| `Ctrl/Cmd+S` | Apply (while editing YAML) |

Right-click a row for its actions.

In a shell, keys go to the shell and nowhere else (including `Esc`, `Tab`,
`Ctrl+K` and `Ctrl+S`). `Ctrl+Shift+C` copies the selection and
`Ctrl+Shift+V` (or `Ctrl+V`) pastes; on macOS `Cmd+C` / `Cmd+V`.
`Shift+PageUp` / `Shift+PageDown` page through the scrollback; double-click
selects a word, triple-click a line. From a shell the command palette is on
`Ctrl+Shift+K` (`Cmd+K` on macOS), and the keyboard returns to the shell
when the palette closes. Click outside the terminal to give the keyboard
back to the rest of the app; choosing a shell's tab gives it to that shell.

Because `Ctrl+V` pastes, programs that use it themselves need their other
binding (in vim, `Ctrl+Q` starts a visual block).

## Layout

```
crates/skiff-core   Kubernetes data layer: sessions, the list/watch engine,
                    logs, exec, port-forward, object operations. No UI code.
crates/skiff-term   The terminal widget used for shells (egui +
                    alacritty's terminal emulation; no PTY, no networking).
crates/skiff        The egui application.
hack/               Local test clusters (kwok) and seed data for development.
```

## Development

`hack/cluster-up.sh` starts two local clusters without Docker (real
kube-apiserver + etcd with [kwok](https://kwok.sigs.k8s.io) simulating
nodes and pods): `demo`, a small varied cluster with broken workloads and a
churn loop, and `scale`, with 100 nodes and ~5,400 pods.

```bash
hack/cluster-up.sh
```

```bash
KUBECONFIG=hack/kubeconfig cargo run --release -- --context demo
```

Tests:

```bash
cargo test
```

The unit tests need nothing; the watch engine's are run against a small
in-process fake API server. The UI tests in `crates/skiff/tests/ui.rs` drive
the real app headlessly against the `demo` cluster and write screenshots to
`target/shots/`, and `crates/skiff-core/tests/live.rs` checks that
background work really stops and reconnects really recover. Both are skipped
when the cluster is not running; rendering uses a software Vulkan driver
(`mesa-vulkan-drivers` on Linux). One UI test covers an account confined to
a namespace; it runs when `SKIFF_TEST_RESTRICTED_KUBECONFIG` points at a
kubeconfig made by `hack/restricted-kubeconfig.sh`.
`crates/skiff/tests/qa_flows.rs` holds a larger, slower set of exploratory
flows that is opt-in (see the note at the top of that file).

Performance numbers come from:

```bash
cargo test --release -p skiff --test perf -- --ignored --nocapture
```

## Not there yet

- Helm releases
- In the terminal: mouse support for programs that ask for it (vim, htop:
  the wheel works, clicks are not passed on), input methods for CJK text,
  and glyphs outside the bundled fonts (CJK and emoji show as boxes)
- Node drain, cron job trigger, bulk actions on multiple rows
- Merging your YAML edits into a newer version of the object: when it
  changed while you were typing you are told, and can load the latest
  version, but your changes have to be made again
- Logs while a node's clock is being corrected: after a backwards step,
  lines can be missed until the clock catches up; after a jump ahead of more
  than a few seconds that is later undone, a reconnect can show a block of
  lines a second time
- Accounts limited to a namespace: tables work once that one namespace is
  selected (`-n <namespace>` or the picker), but the overview page and
  selecting several namespaces still need cluster-wide list rights

## License

MIT. Bundled fonts: Inter and JetBrains Mono (SIL Open Font License), Material
Symbols (Apache 2.0).
