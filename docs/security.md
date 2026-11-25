# What Skiff does and does not do

Skiff holds the same power over your clusters as your kubeconfig gives
`kubectl`. This page says what it does with that, what it connects to,
and where its limits are. Each statement is about the code in this
repository; where something was measured rather than read, the
measurement is in [benchmarks.md](benchmarks.md#what-skiff-does-and-does-not-do).

Skiff is new and has had no independent security review. To report a
problem, see [SECURITY.md](../SECURITY.md).

## Network destinations

Skiff connects to two places.

1. **The API server of the cluster you open**, at the address in your
   kubeconfig, through the proxy named there or in `HTTPS_PROXY` if any.
   Everything about the cluster goes over this connection, including
   logs, shells, port-forwards, and Prometheus queries, which the API
   server forwards to the Prometheus Service
   ([observability.md](observability.md#the-permission-it-needs)).
   Credential plugins named in your kubeconfig are started as `kubectl`
   starts them, and connect wherever they connect (your cloud's login
   service, for instance).
2. **`api.github.com`**, to ask whether a newer release exists: once at
   startup and every four hours while the app is open. The request is
   made by the system's `curl`, is anonymous, and carries nothing about
   you or your clusters beyond what any HTTPS request does. Turn it off in
   Settings → Updates, with `"updates": {"check": false}`, or with
   `SKIFF_NO_UPDATE_CHECK=1`. On macOS, choosing **Update** also downloads
   the new app from GitHub.

A third destination exists only when you set one: a Prometheus outside
the cluster (`clusters.<context>.prometheus.url`), queried directly over
HTTPS. Its credential is obtained when needed by a program you choose or
by your cloud's CLI (`az`, `aws`, `gcloud`), or read from an environment
variable or file you name; it is held in memory only and never written by
Skiff, and it is sent only over HTTPS (or to this computer)
([observability.md](observability.md#prometheus-outside-the-cluster)).

Links (a release page, the project page, your Grafana) are opened in your
browser, not fetched by Skiff.

Skiff listens on no network port. The one exception is a port-forward you
start: it listens on `127.0.0.1` only, for as long as it is open, and
accepts at most 256 connections at a time.

There is **no telemetry**: no usage statistics, no crash reports, no
identifier. There is no account and nothing to sign in to. No web view
is embedded, and neither `kubectl` nor `helm` is bundled; your own
`kubectl` is run only when you choose "open in terminal app".

## Credentials

- Skiff reads kubeconfig files and never writes to them. Adding a
  kubeconfig in the app records its *path* in `config.json`.
- `config.json` and `keys.json` hold no secrets. `grafana_url` is
  rejected if it contains a user name or password.
- When a credential plugin fails, Skiff shows its exit status and the last
  three lines of its error output, cleaned of control characters. Its
  standard output, where the token is printed, is never shown or logged.
- Started from a launcher, Skiff asks your login shell for `PATH`,
  `KUBECONFIG` and the proxy variables only. The rest of the shell's
  environment (cloud profiles, tokens) is deliberately not imported, and
  `PATH` entries that are not absolute are dropped, so a plugin is never
  looked up relative to the current folder.

## Secrets in the cluster

- Tables are fetched with the server-side Table API, the one `kubectl get`
  uses: the server sends rows and object metadata, not objects. The
  values of Secrets are not requested for a list.
- One limit: metadata includes annotations, so a Secret created with
  `kubectl apply` carries its values in the
  `kubectl.kubernetes.io/last-applied-configuration` annotation, and the
  server does send that.
- Opening a Secret fetches it in full, as showing it requires. Values are
  hidden until you reveal them.
- A value or a Secret's YAML that you copy with Skiff's copy button is
  cleared from the clipboard after 60 seconds, unless you copied something
  else since. Skiff reads the clipboard once, at that moment, to tell: if
  it holds anything other than the copied value, in Skiff or in another
  app, it is left alone. If the clipboard cannot be read, it is cleared
  unless something else was copied in Skiff. Two gaps: text you select and copy by hand from a
  revealed value is not tracked, and a secret still on the clipboard when
  you quit Skiff within those 60 seconds stays there.

## What a cluster sends is not trusted

Object names, log lines and terminal output come from the cluster, and a
cluster can be hostile or simply broken.

- **Logs.** Escape sequences are removed from log lines before they are
  shown (colours are kept as colours). A line is cut at 64 KiB, and a log
  view keeps at most 100,000 lines or 128 MiB.
- **The built-in terminal.** What a container prints goes through a
  terminal emulator that is bounded: grid and scrollback (10,000 lines),
  the length of escape strings, stacked combining characters, and pending
  replies. A request from the container to write or read your clipboard
  (OSC 52) is ignored. Pasted text cannot break out of a bracketed paste.
  If the emulator fails on some input, that one terminal is reset; the
  app keeps running.
- **Your own terminal app.** For "open in terminal app" the `kubectl`
  command is built as an argument list. On macOS and Linux it is written,
  quoted, to a script only you can read in a fresh private folder, and
  the terminal is asked to run only that script. On Windows names with
  characters `cmd` cannot quote safely are refused.
- **Sizes.** API responses, watch events and Prometheus answers have size
  limits. An oversized watch event is skipped and the watch continues; an
  oversized response is an error message, not a crash.
- **Queries.** Names that go into a Prometheus query or a URL are escaped.
- **TLS.** Connections use rustls; OpenSSL is not linked. A context with
  `insecure-skip-tls-verify` is marked "TLS not verified".

## Changing things

- Applying a manifest uses server-side apply without `force`: if a field
  is managed by something else (Helm, a controller), nothing is
  overridden silently. Skiff names the fields and their managers and
  offers "Apply anyway".
- Deleting always asks. With **Ask before changes** on for a cluster, so
  do restart, scale, cordon, suspend and apply.
- **Read-only** makes Skiff refuse every change to a cluster, including
  new shells and port-forwards (ones that are already open keep running
  until you close them). If the config file that holds this setting
  cannot be read, every cluster is treated as read-only. It is a guard
  against mistakes inside Skiff, not access control; see
  [clusters.md](clusters.md#read-only-clusters).

## Updates

- **Windows and Linux:** Skiff never installs anything. It shows that a
  new version exists and opens the release page in your browser.
- **macOS:** the new app is downloaded into a private folder next to the
  installed one. Before anything in it is run, and again immediately
  before the swap, its code signature is verified
  (`codesign --verify --deep --strict`). A build signed with an Apple
  Developer ID additionally requires the update to be signed by the same
  team with Skiff's bundle identifier, and refuses it otherwise.
- The download must contain the Skiff app and nothing else, and the
  version and build number in its signed `Info.plist` must be the ones
  the release announced and newer than the running copy. An older Skiff,
  even a genuine one, is not installed.
- Released macOS builds are signed with the owner's Developer ID (Team
  ID `2632424VR2`) and notarized, so the team check above applies to
  them. **Limit:** for a copy without a Developer ID signature (one you
  compiled yourself, or an old unsigned prerelease) the check only
  confirms that the downloaded app is intact, not who made it; what
  stands behind the download is then HTTPS to GitHub and this
  repository's release process.
- Update requests go only to `https://api.github.com/`, and `curl` is
  told to ignore your own `~/.curlrc` for them. Official builds
  never send a token. A build compiled for another repository
  (`SKIFF_UPDATE_REPO`, for a private fork) retries with the token from
  `$GITHUB_TOKEN`, `$GH_TOKEN` or `gh auth token` only after GitHub refused
  the anonymous request, and hands it to `curl` on standard input, not on
  the command line.
- **Channels.** *Stable* offers only releases tagged `vX.Y.Z`. *Nightly*
  offers every build of `main`. Neither offers a build older than the one
  running.

## How releases are made

- Releases are built by the workflow in `.github/workflows/release.yml`
  from this repository, with every action pinned to a commit.
- Each release has a `SHA256SUMS` file and build provenance
  attestations: see [install.md](install.md#checking-a-download). (The
  workflow skips attestations only while a repository is private.)
- The macOS build is signed with an Apple Developer ID and notarized.
  The Windows build is not yet signed, and Linux packages carry no
  signature. The [code signing policy](code-signing-policy.md) names who
  may release and what is planned for Windows.
- Signing keys are available only to builds of `main` and of `v*` tags,
  through a protected environment.
- CI runs `cargo-deny` on every change and weekly: known advisories,
  licences, and that every crate comes from crates.io. Dependabot
  proposes dependency updates.

## Not claimed

That Skiff has no vulnerabilities; anything about the security of your
cluster, your credential plugins or your terminal app; and protection
against someone who can already change files in your home folder.
