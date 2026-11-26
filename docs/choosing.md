# Choosing a Kubernetes desktop client

There are four kinds of program for looking at a cluster from your own
machine, and each is the right choice for somebody. This page says what
each kind is good at, where Skiff fits, and when something else will
suit you better. As everywhere in this repository, other programs are
described by what kind of program they are, not by name
([why](benchmarks.md#why-the-rows-have-no-product-names)).

## The four kinds

| | Electron-based desktop client | Web dashboard | Terminal UI | Native desktop client (Skiff) |
| --- | --- | --- | --- | --- |
| **What it is** | A web app packaged with its own copy of Chromium | A web app, run in the cluster or locally, used in a browser | A full-screen program inside your terminal | One compiled program that draws its own window |
| **Typically good at** | Extensions and plugins, a large feature set, long track record | Sharing one view with a team, no install on each laptop | Speed of keyboard use, works over SSH, tiny install | Low memory and CPU, fast start, large clusters, nothing in between you and the API server |
| **Typical costs** | Several processes and hundreds of MiB, an embedded browser to keep patched | Something to deploy and secure, its own sign-in in front of the cluster | No charts or mouse-driven views, the terminal's limits | Fewer features than older, larger projects; no plugin system |
| **Account or sign-in of its own** | Varies; some ask for one | Usually | No | No |

What the architectures cost was measured once, side by side, on one Mac:
[Comparison with similar software](benchmarks.md#comparison-with-similar-software).
There, with each program's pod list open on a cluster of 5,415 pods, Skiff
used 146 MiB, a terminal UI client 397 MiB and an Electron-based client
893 MiB of memory.

## Skiff is a good fit if

- you already have a working kubeconfig, and want a window that opens at
  once and stays quick with thousands of pods;
- you would rather nothing sat between you and the API server: no
  account, no telemetry, no background service, no port opened unless
  you start a port-forward ([security](security.md));
- you want metrics and alerts from the Prometheus you already run,
  without a second login ([metrics and alerts](observability.md));
- you look after production and want a hard read-only switch per
  cluster ([clusters](clusters.md#read-only-clusters));
- you like the keyboard: a command palette, filters by column, Vim-style
  keys ([keyboard](keyboard.md)).

## Something else will suit you better if

- **you depend on extensions or plugins.** Skiff has no plugin system, on
  purpose: it is one small program
  ([what gets in](https://github.com/TsenkoTsenkov/skiff/blob/main/CONTRIBUTING.md#what-gets-in)).
- **you manage Helm releases from the GUI.** Not there yet; it is on the
  [roadmap](https://github.com/TsenkoTsenkov/skiff#roadmap).
- **you need node drain, bulk actions on many rows, or log search across
  history** (Loki, Elasticsearch). Also on the roadmap.
- **your team wants one shared, browser-based view of a cluster.** A web
  dashboard deployed in the cluster does that; Skiff is a program for
  your own machine.
- **you work only over SSH on remote hosts.** A terminal UI runs there;
  Skiff needs a desktop.

## Trying it next to what you use now

Skiff reads the same kubeconfig as `kubectl` and every other client, and
changes nothing in it unless you add a file in Settings. It writes its own
settings to its own folder ([configuration](configuration.md)), so it can
run next to any other client and be removed without a trace. Install it
([install](install.md)), open it, and pick a cluster.
