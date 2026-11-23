# Development

How the code is laid out, how to run it against local test clusters, and
how tests, screenshots, benchmarks and releases work. What gets accepted
is in [CONTRIBUTING.md](../CONTRIBUTING.md).

## Layout

```
crates/skiff-core   Kubernetes data layer: kubeconfig loading, sessions, the
                    list/watch engine, logs, exec, port-forward, object
                    operations, the Prometheus client. No UI code.
crates/skiff-term   The terminal widget used for shells (egui + alacritty's
                    terminal emulation; no PTY, no networking).
crates/skiff        The egui application: views, settings, config file,
                    updater, and the binary's entry point.
hack/               Local test clusters, seed data, packaging scripts,
                    screenshots, benchmarks.
packaging/          Installer and package metadata; packaging/README.md
                    explains releases and signing.
site/               The project's web page (static HTML and CSS).
docs/               These documents.
```

Design in a few lines:

- The UI is immediate-mode (egui) and repaints only when data changes or
  a relative time on screen would tick over.
- A resource type is listed and watched when you open it and stops three
  minutes after you leave it.
- Lists use the server-side Table API, like `kubectl get`: the server
  renders the columns, so every resource type works without a client-side
  schema. Full objects are fetched only when one is opened.
- `skiff-core` knows nothing about the UI. It publishes immutable
  snapshots; the UI thread never waits on the network or on a lock.

## Build and run

Rust 1.95 or newer; `rust-toolchain.toml` pins the version CI and
releases use, and `rustup` installs it on first use. Platform
prerequisites are in [install.md](install.md#building-from-source).

```bash
cargo run --release
```

There is no `rustfmt.toml` and the code is not rustfmt-clean (it is
hand-formatted with wider lines). Do not run `cargo fmt` on the
workspace; match the surrounding code.

## The test bed

`hack/cluster-up.sh` starts two local clusters without Docker: a real
`kube-apiserver` and `etcd`, with [kwok](https://kwok.sigs.k8s.io)
simulating nodes and pods.

| Context | What it is |
| --- | --- |
| `demo` | 10 nodes and about 150 pods in a dozen namespaces, with broken workloads (crash loops, bad config, pending pods), custom resources, simulated logs, a port-forward target, metrics-server, a **fake Prometheus**, and a churn loop that keeps changing things |
| `scale` | 100 nodes and about 5,400 pods, for performance work |

```bash
hack/cluster-up.sh
KUBECONFIG=hack/kubeconfig cargo run --release -- --context demo
```

- It needs Linux on x86_64, `curl`, Python 3 and Go (kwok and
  metrics-server are built with `go install`); it downloads the
  Kubernetes and etcd binaries on first use and installs `kwokctl`,
  `kwok` and `kubectl` into `/usr/local/bin` if they are missing.
- `SKIP_SCALE=1 hack/cluster-up.sh` starts only `demo`.
  `hack/status.sh` shows what is running; `hack/cluster-down.sh` stops
  everything. The script is safe to run again.
- The kubeconfig is `hack/kubeconfig`; cluster state lives in
  `hack/.state/`.
- `hack/churn.sh stop` pauses the changes (`start` resumes them), which
  helps when a test compares counts.

**The fake Prometheus.** kwok pods are not real, so nothing could scrape
them. `hack/fake-prometheus.py` (standard library only) serves the three
endpoints Skiff uses, `/api/v1/query`, `/api/v1/query_range` and
`/api/v1/alerts`, with deterministic synthetic data for the objects that
exist in the `demo` cluster, behind the Service
`monitoring/prometheus-operated`. Skiff reaches it through the API
server's service proxy exactly as it would a real one. Paths under
`/deny/` answer 403 and under `/broken/` 500, for the error-handling
tests. Because it was written to match the queries Skiff sends, it proves
the plumbing, not that the queries are right for a real Prometheus.

**A namespace-limited account.** `hack/restricted-kubeconfig.sh <file>`
writes a kubeconfig for a service account that may only read the `shop`
namespace of `demo` (context `restricted`).

## Tests

```bash
cargo test --workspace
```

| Tests | Need | What they do |
| --- | --- | --- |
| Unit tests (`--lib`) | nothing | Logic of each crate. The watch engine, logs, apply and the Prometheus client run against small in-process fake API servers. |
| `crates/skiff/tests/ui.rs` | the `demo` cluster, a Vulkan driver | Drive the real app headlessly and write screenshots to `target/shots/`. |
| `crates/skiff-core/tests/live.rs`, `prom_live.rs` | the `demo` cluster | Background work really stops, reconnects recover, Prometheus is found and queried through the proxy. |
| `crates/skiff/tests/qa_flows.rs` | the clusters | A larger, slower exploratory set; opt-in, see the top of the file. |
| `crates/skiff/tests/perf.rs` | the `scale` cluster | In-process performance numbers; `--ignored`, see below. |

The cluster tests look for `hack/kubeconfig` (or the file in
`SKIFF_TEST_KUBECONFIG`; the context in `SKIFF_TEST_CONTEXT`, default
`demo`) and skip themselves when it is not there, so `cargo test` passes
on a machine without the test bed, which is how CI runs it. The
restricted-account test runs when `SKIFF_TEST_RESTRICTED_KUBECONFIG`
names a file made by the script above.

Rendering in the UI tests uses whatever Vulkan driver the machine has; on
a machine without a GPU install a software one (`mesa-vulkan-drivers`).
A few UI tests compare counts while the churn loop runs and can fail by
bad luck; run the one test again before suspecting your change.

Before a pull request:

```bash
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo check -p skiff --target x86_64-pc-windows-gnu   # if you touched platform code and have mingw
```

CI (`.github/workflows/ci.yml`) runs clippy, the tests on Linux, a
compile check and the unit tests on Windows and macOS, `cargo-deny`
(advisories, licences, crate sources; rules in `deny.toml`), and checks
of the packaging files.

## Screenshots

The images in the README, the docs and the site are produced by the UI
tests.

```bash
hack/screenshots.sh          # run the UI tests against demo, then copy
hack/screenshots.sh --copy   # only copy from target/shots/
```

It copies a fixed list of screenshots to `docs/img/` and `site/img/`
under stable names, reduced to a 256-colour palette, and cuts the details
the site shows next to its text (`site/img/crop-*.png`; needs Python with
Pillow). To publish a different view, add it to the `picks` or `crops`
list in the script. When you change
how something looks, look at the affected files in `target/shots/` in
both themes; the names say which test wrote them.

## Benchmarks

Two kinds of numbers exist, and they are not the same:

- **The launched binary, measured from outside**: `hack/bench/run.sh`.
  This is what [benchmarks.md](benchmarks.md) reports, and
  [hack/bench/README.md](../hack/bench/README.md) explains every figure.
  The same script measures any other app.
- **In-process**, without a window:

  ```bash
  cargo test --release -p skiff --test perf -- --ignored --nocapture
  ```

  Useful for spotting a regression in frame cost, filtering or sorting
  while developing. `SKIFF_PERF_CONTEXT` chooses the context (default
  `scale`).

`SKIFF_TRACE_STARTUP=1` makes the app print when its first frame was
built and when the first table was fully loaded.

Only publish numbers you measured, with the machine and the method next
to them.

## Icons

`crates/skiff/assets/icons/skiff.svg` is the source. `hack/icons.sh`
regenerates the PNG sizes, `skiff.ico` and `Skiff.icns` from it (needs
`rsvg-convert` and Python with Pillow).

## Releases

A push to `main` publishes a prerelease `build-<N>` (the *nightly*
channel); a tag `vX.Y.Z` publishes a stable release. Both are made by
`.github/workflows/release.yml`, which builds, packages, optionally
signs, checksums and publishes for macOS, Windows and Linux. The owner's
checklist is [releasing.md](releasing.md); the pipeline, the signing
secrets and the package managers are described in
[packaging/README.md](../packaging/README.md).

When your change is something a user will notice, add a line to the
"Unreleased" section of [CHANGELOG.md](../CHANGELOG.md).
