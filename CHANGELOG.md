# Changelog

What changed in Skiff, for the people who use it. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions
follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Stable releases are tagged `vX.Y.Z`. Builds of the main branch between
them are published as prereleases named `build-N` and are not listed
here.

## [Unreleased]

### Added

- **What's new**: after an update Skiff lists what changed in the new
  version, once. It can be opened again from the palette ("What's new")
  and from Settings, Updates. The Update button's tooltip and the release
  page on GitHub show the same list before you update.
- **An Apple Silicon-only download for macOS**, `Skiff-macos-arm64.dmg`:
  the native arm64 build without the Intel half, so about half the size
  of the universal disk image. The universal image stays, and both update
  themselves the same way.
- **Motion.** The detail panel slides in from the right and out again,
  the logs and shells panel slides up and down, and notifications fade
  in and out instead of popping. Dialogs fade in a little faster than
  before. Nothing waits for an animation: each takes 0.15 s, and what is
  on its way out no longer reacts to keys or clicks.
- **Settings, Appearance: Animations** (`animations` in `config.json`)
  turns all of that off, along with the animated scrolling to the
  selected row, for anyone whom movement on screen bothers.

### Changed

- The cluster's version in the top bar reads "Kubernetes v1.32.3", so it
  is not taken for Skiff's own version (which is in Settings, About).

## [0.1.1] - 2026-11-25

### Added

- **Column filters** in the table filter: `node:worker-1`, `status:crash`,
  `ns:kube`, `label:app=web`. A column title can be shortened to any
  prefix, `column:=value` matches the whole value and a leading `!` leaves
  the matches out.
- **Find shortcuts**: `Ctrl/Cmd+F` filters the table, or finds in the log
  tab you are working in. Shells search their scrollback with `Cmd+F` on
  macOS and `Ctrl+Shift+F` elsewhere.
- **"Show pods"** on a node or a namespace (right-click, or the node's
  detail panel) opens the pods table filtered to it.
- **Settings**: log history for new log tabs (`log_history` in
  `config.json`) and "Reset all" table columns in Appearance; in Clusters,
  each cluster's connection state with open, reconnect, disconnect and
  hide from the cluster menu (`clusters.<context>.hidden`), and the
  kubeconfig files found in `~/.kube` offered next to "Add file…"; frame
  time, memory and open connections in About.

### Changed

- **Table columns** are sized to their content, and the less important
  ones drop out when the window is narrow. Dragging a column edge works
  like a splitter and holds; double-click resets it. The columns and
  widths you choose are remembered between runs.

### Fixed

- On macOS the Dock shows Skiff's own icon while it runs, not the egui
  logo.
- The overview no longer says "all ready" or "nothing is failing" for a
  list that could not be loaded, or while lists are still loading.
- A Secret's overview no longer shows the
  `kubectl.kubernetes.io/last-applied-configuration` annotation, which can
  hold the secret's values in plain text.

## [0.1.0] - 2026-11-24

The first stable release, and the first version meant for people other
than its authors.

### Added

- **Documentation site** at <https://tsenkotsenkov.github.io/skiff/docs/>:
  the pages in `docs/` with navigation, built by `hack/site-build.py`.
  New pages: a [keyboard reference](docs/keyboard.md) and
  [troubleshooting and FAQ](docs/troubleshooting.md).
- **A demo film** of the app on the README and the site, recorded from
  the real program against the local test cluster.
- **Support Skiff** link in Settings → About, and a Sponsor button on the
  repository: both lead to Buy Me a Coffee. The app itself contacts
  nothing new; the link opens in your browser.
- **Downloads for Windows and Linux**, next to macOS: a per-user Windows
  installer and a portable zip; AppImage, `.deb` and `.tar.gz` for x86_64
  and arm64. The macOS app is now one universal build for Apple Silicon
  and Intel. Every release has a `SHA256SUMS` file. See
  [docs/install.md](docs/install.md).
- **Metrics tab** for pods, workloads and nodes: CPU, memory, network and
  restart charts over 1 hour to 7 days, from the Prometheus in your
  cluster. It is reached through the Kubernetes API server, so no extra
  credentials or port-forward are needed. Without Prometheus, a short CPU
  and memory history from metrics-server is shown instead.
- **Firing alerts** from Prometheus on the overview, in the top bar, and
  on the object each alert is about.
- **Open in Grafana** on the Metrics tab, when you set your Grafana's
  address for a cluster.
- **Read-only clusters**: mark a cluster read-only and Skiff refuses
  every change to it, shells and port-forwards included. **Ask before
  changes** makes restart, scale, cordon, suspend and apply confirm
  first.
- **Settings window** (`Ctrl+,` / `Cmd+,`): theme, zoom, kubeconfig
  files, per-cluster protection, update channel, terminal app, the config
  files, and an About page with version details to copy into a bug
  report.
- **A config file**, `config.json`, for everything in Settings and for
  the per-cluster Prometheus and Grafana settings. Mistakes in it are
  reported and cost only the key they are in. See
  [docs/configuration.md](docs/configuration.md).
- **Welcome page** when no kubeconfig is found: it shows where Skiff
  looked and lets you add a file, also by dropping it on the window.
- **Several kubeconfig files**: a list of files, folders and wildcards in
  the config file, `--kubeconfig` on the command line, "Add kubeconfig
  file…" and "Reload kubeconfig" in the cluster menu and the palette.
  Changed files are picked up when you switch back to Skiff. In the list,
  `"$KUBECONFIG"` stands for kubectl's own locations, so adding a file
  keeps them, and keeps following your environment.
- **A connection page** when a cluster cannot be reached: what went
  wrong, what to do, Retry, and details to copy. A missing credential
  plugin is named, with the kubeconfig's install hint; an expired login
  shows the command to run.
- **Keyboard shortcut list** on `?`; the key can be rebound as `help`.
- **Zoom** with `Ctrl/Cmd` and `=`, `-`, `0`, from 50% to 300%.
- **A theme that follows the system's** light or dark mode.
- **Update check on Windows and Linux**: a "New version" button opens the
  release page. A choice of **stable or nightly** updates.
- An app icon; the window title names the cluster.
- Shells and log tails can open in more terminal apps: Ghostty, WezTerm,
  kitty and Alacritty on macOS by setting, Windows Terminal on Windows,
  and more Linux terminals. The choice is a setting (`terminal`).
- `--version` and `--help` work from a Windows terminal.
- A badge on clusters whose kubeconfig turns TLS verification off.

### Changed

- **Applying a manifest no longer overrides fields owned by something
  else** (Helm, a controller). Skiff shows which fields conflict and who
  manages them, and "Apply anyway" does what used to happen silently.
- **A broken or missing kubeconfig file no longer hides the clusters in
  the other files.** The broken file is named with the reason.
- **Accounts limited to one namespace** can use the overview: it offers
  to show that namespace instead of an error.
- Text contrast is higher in both themes, and the control that has the
  keyboard is outlined.
- On macOS and Linux, Skiff started from the Dock or a desktop launcher
  now also takes `KUBECONFIG` and the proxy variables from your login
  shell, not only `PATH`. Linux launchers are covered too.
- The config folder on Windows is `%APPDATA%\skiff`. A `keys.json` in the
  old place (`%USERPROFILE%\.config\skiff`) is still read.
- The macOS download is named `Skiff-macos-universal.dmg`; file names no
  longer contain the version, so links to the latest release stay valid.
- The app identifier is `io.github.tsenkotsenkov.skiff`. Saved window
  state is kept.
- On a machine where the default renderer cannot start, Skiff falls back
  to OpenGL instead of failing. `SKIFF_RENDERER` forces one.
- A copied Secret value is cleared from the clipboard after 60 seconds,
  unless something else has been copied by then, in Skiff or elsewhere.

### Security

- **macOS updates are verified before anything in them runs**: the
  downloaded app's code signature is checked, and a build signed with a
  Developer ID accepts only updates signed by the same team.
- The updater **never sends a GitHub token** from official builds, and
  talks only to `api.github.com`.
- The stable channel ignores prereleases, so a build of the main branch
  is never installed over a release unless you choose the nightly
  channel.
- "Open in terminal app" no longer types a command line into your shell:
  it runs a private script with the cluster's names passed as quoted
  arguments, so a hostile pod or context name cannot run a command.
- Memory is bounded against what a cluster sends: very long log lines,
  huge watch events and responses, and floods of port-forward
  connections.
- A credential plugin's output (where the token is) is never shown in
  error messages.
- Release builds come from one workflow with pinned actions, checksums
  and build provenance attestations
  ([checking a download](docs/install.md#checking-a-download)).
- **The macOS build is signed with an Apple Developer ID and notarized by
  Apple**; it opens without a first-launch warning.
- A [code signing policy](CODE_SIGNING_POLICY.md) says who may release
  Skiff, where the binaries are built and what the program sends over the
  network.

### Known limits

- The Windows build is not yet signed: Windows asks for confirmation on
  first launch ([docs/install.md](docs/install.md#windows)). Linux
  packages carry no signature.
- The Windows build and the new installers and packages have had no use
  yet beyond automated checks.
- Metrics queries have been tested against the project's simulated
  Prometheus, not yet against the many real installations.

## Before 0.1.0

Earlier builds were published for macOS only, as `build-N`, without
version numbers or a changelog. They had the resource browser, details,
YAML editing, logs, the built-in shell, port forwarding, the command
palette, vim-style keys with custom bindings, and in-app updates on
macOS.

[Unreleased]: https://github.com/TsenkoTsenkov/skiff/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/TsenkoTsenkov/skiff/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/TsenkoTsenkov/skiff/releases/tag/v0.1.0
