# Contributing to Skiff

Bug reports, fixes and well-scoped features are welcome. This page says
how to build and test, what a pull request should look like, and, just as
important, what kind of change gets in.

By taking part you agree to the [code of conduct](CODE_OF_CONDUCT.md).
Security problems go through [SECURITY.md](SECURITY.md), not the issue
tracker.

## Reporting a bug

Use the bug form in the issue tracker. The most useful reports say:

- the Skiff version and build (Settings → About has a copy button, or
  `skiff --version`) and the operating system;
- the kind of cluster (EKS, GKE, AKS, kind, k3s, …) and how you sign in
  (credential plugin, OIDC, client certificate);
- what you did, what you expected and what happened; for a connection
  problem, the text from **Copy details** on the connection page.

Leave out tokens, kubeconfig contents and Secret values.

## Building and running

```bash
cargo build --release
./target/release/skiff
```

You need Rust 1.95 or newer; prerequisites per system are in
[docs/install.md](docs/install.md#building-from-source). The layout of
the code is in [docs/development.md](docs/development.md).

## The test bed

You do not need a real cluster. On Linux:

```bash
hack/cluster-up.sh
KUBECONFIG=hack/kubeconfig cargo run --release -- --context demo
```

starts two local clusters without Docker (a real API server with
simulated nodes and pods): `demo`, small and varied, with broken
workloads, simulated logs, a fake Prometheus with charts and alerts, and
a loop that keeps changing things; and `scale`, with 100 nodes and about
5,400 pods. `hack/cluster-down.sh` stops them. Details:
[docs/development.md](docs/development.md#the-test-bed).

## Tests

```bash
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

- Unit tests need nothing.
- The UI tests (`crates/skiff/tests/ui.rs`) drive the real app against
  the `demo` cluster and write screenshots to `target/shots/`; they skip
  themselves when the test bed is not running. If you change how
  something looks, look at the screenshots, in both themes.
- Add tests for the logic you add. Code that handles what a cluster or a
  file sends should have a test with hostile input.
- The code is hand-formatted with wide lines and there is no rustfmt
  configuration: match the surrounding code and do not reformat files you
  are not changing.

## What gets in

Skiff's point is to be small, fast and trustworthy with cluster
credentials. Every feature is weighed against that, and "no" is a common
and friendly answer. Before writing a feature, open an issue or a
discussion and talk it through.

A change fits when it:

- helps with looking after a cluster through the Kubernetes API, for many
  users rather than one setup;
- needs no new stored credential, no account, no background service and
  no web view;
- keeps the app fast: nothing is fetched or watched that is not on
  screen, and nothing blocks the UI thread;
- adds no dependency, or a small pure-Rust one whose need is explained in
  the pull request (no OpenSSL, no cmake, no C++; `deny.toml` enforces
  part of this);
- works on macOS, Windows and Linux, or says clearly why it cannot.

Decided against, so please do not send these:

- a plugin or extension system;
- embedded web content (Grafana dashboards, documentation panes);
- Skiff's own sign-in, SSO or VPN handling: kubeconfig credential plugins
  are the interface;
- incident management: acknowledging, silencing or routing alerts;
- a hosted, web or team-sync mode;
- bundling `kubectl`, `helm` or other programs;
- telemetry of any kind, including opt-in.

On the list for later, where help is welcome: Helm releases (read-only
first), node drain, actions on several rows, log search across history.
The README's roadmap has the current list.

## Pull requests

- One change per pull request, small enough to review in one sitting.
  Refactoring separately from behaviour changes.
- Say what a user will notice and why the change is needed; link the
  issue.
- Say how you checked it: tests added or run, which systems and clusters
  you tried, and what you could not try. "Not run on Windows" is useful
  to know and no reason for shame.
- Attach a screenshot for anything visible.
- User-facing wording is short and plain: what happened and what to do
  next.
- Update the docs your change makes untrue, and add a line to
  "Unreleased" in [CHANGELOG.md](CHANGELOG.md) when a user would notice.
- CI must pass: clippy without warnings, the tests, and `cargo-deny`.

Contributions are accepted under the project's [MIT licence](LICENSE).
