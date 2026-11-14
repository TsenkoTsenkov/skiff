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
| Binary | 31 MB, no runtime dependencies beyond a GPU driver |

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
- YAML view with syntax highlighting; edit and apply with conflict detection;
  create or update objects from a manifest (server-side apply)
- Pod and workload logs: follow, multi-pod merge, filter, timestamps, wrap,
  previous container, level colouring
- Port forwarding for pods and services, with a list of active forwards
- Scale, rollout restart, cordon / uncordon, suspend / resume cron jobs,
  delete (optionally forced)
- CPU / memory columns and cluster usage from metrics-server (no Prometheus)
- Cluster overview: what is broken right now and recent warnings
- Command palette (`Ctrl/Cmd+K`): jump to any kind (kubectl short names
  work: `po`, `deploy`, `svc`, …), any loaded object by name, namespace,
  cluster or action
- Dark and light themes

## Build and run

Requirements: Rust 1.95 or newer.

```bash
cargo build --release
```

```bash
./target/release/skiff
```

Options: `--context <name>`, `--view <resource>` (e.g. `--view pods`),
`--light`. Clusters come from `$KUBECONFIG` or `~/.kube/config`; exec-based
auth plugins (EKS, GKE, …) and OIDC are supported.

Linux needs a Vulkan or OpenGL driver and `libxkbcommon` (on Debian/Ubuntu:
`libxkbcommon-x11-0`, plus `mesa-vulkan-drivers` if you have no GPU driver).
macOS and Windows need nothing extra.

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

## Layout

```
crates/skiff-core   Kubernetes data layer: sessions, the list/watch engine,
                    logs, exec, port-forward, object operations. No UI code.
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

The UI tests in `crates/skiff/tests/ui.rs` drive the real app headlessly
against the `demo` cluster and write screenshots to `target/shots/`, and
`crates/skiff-core/tests/live.rs` checks that background work really stops
and reconnects really recover. Both are skipped when the cluster is not
running; rendering uses a software Vulkan driver (`mesa-vulkan-drivers` on
Linux). `crates/skiff/tests/qa_flows.rs` holds a larger, slower set of
exploratory flows that is opt-in (see the note at the top of that file).

Performance numbers come from:

```bash
cargo test --release -p skiff --test perf -- --ignored --nocapture
```

## Not there yet

- Helm releases
- Node drain, cron job trigger, bulk actions on multiple rows
- Namespace-scoped watches for accounts that cannot list cluster-wide (the
  data layer supports them; the UI does not switch to them yet)

## License

MIT. Bundled fonts: Inter and JetBrains Mono (SIL Open Font License), Material
Symbols (Apache 2.0).
