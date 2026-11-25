# Code signing policy

This page says who may change Skiff, where the published programs are
built, which of them carry a code signature today, what the program sends
over the network, and how to report a problem with a release. The same
text is in
[CODE_SIGNING_POLICY.md](https://github.com/TsenkoTsenkov/skiff/blob/main/CODE_SIGNING_POLICY.md)
at the top of the repository.

## What is signed today

| System | State |
| --- | --- |
| macOS | Signed with the owner's Apple Developer ID (`Developer ID Application: Tsenko Tsenkov`, Team ID `2632424VR2`) and notarized by Apple. |
| Windows | **Not signed.** An application for free code signing through the SignPath Foundation is being made. Until it is granted, the installer and `skiff.exe` carry no signature and Windows SmartScreen asks before running them. |
| Linux | The packages carry no signature. |

Every release, on every system, has a `SHA256SUMS` file and, from 0.1.0
on, a build provenance attestation for each file (prereleases made while
the repository was private have none). [Checking a
download](install.md#checking-a-download) shows how to verify both. The
notes of each release state whether that build is signed; the statement
is derived from the built files.

> **Once approved (not yet the case).** If the SignPath Foundation accepts
> the application, the Windows installer and `skiff.exe` will be signed
> through SignPath, and this section will then read: "Free code signing
> provided by [SignPath.io](https://about.signpath.io), certificate by
> [SignPath Foundation](https://signpath.org)." Nothing is signed by
> SignPath today.

## Roles

Skiff has one maintainer, so one person holds every role.

| Role | Who | What the role may do |
| --- | --- | --- |
| Committers and reviewers | [TsenkoTsenkov](https://github.com/TsenkoTsenkov), the repository owner | Write to the repository; review and merge pull requests. |
| Approvers | [TsenkoTsenkov](https://github.com/TsenkoTsenkov), the repository owner | Decide that a release is made, by pushing its `vX.Y.Z` tag; approve signing requests once Windows signing exists. |

Changes from anyone else arrive as pull requests and are merged only by
the owner after review ([CONTRIBUTING.md](../CONTRIBUTING.md)). Under
this policy nobody else is given write access to the repository or to its
signing secrets, and anyone who is later given a role is added to this
table first. Every account holding a role must use two-factor
authentication on GitHub and on any signing service.

## Where the binaries come from

- The published files are built **only by GitHub Actions**, by
  [`.github/workflows/release.yml`](../.github/workflows/release.yml) in
  this repository, on GitHub-hosted runners. No release file is built or
  signed on anyone's own machine.
- A stable release is built from a commit of the `main` branch whose
  version in `Cargo.toml` has not been released yet, and its `vX.Y.Z` tag
  is made on that commit; or from a pushed `vX.Y.Z` tag, which must equal
  the version in `Cargo.toml` or the build stops. Other builds of `main`
  are published as prereleases named `build-N` and are never marked as
  the latest release.
- Only Skiff's own program is signed. It is compiled from this
  repository's source and the crates pinned in `Cargo.lock`, all of which
  come from crates.io. Two things in the packages are not compiled here:
  the AppImage starts with the AppImage project's runtime, fetched at a
  pinned checksum, and the Windows installer is made with NSIS.
  [THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md) lists the bundled
  fonts and crates.
- The signing material is kept in a GitHub environment that only builds
  of `main` and of `v*` tags can use. The program is compiled before the
  keys are put on the build machine, and a build that is not published is
  never given them.
- Every action the workflow uses is pinned to a commit.
- The attestation of each file records the workflow, the commit and the
  tag it was built from; `gh attestation verify <file> -R
  TsenkoTsenkov/skiff` checks it.

## Privacy

Skiff has no account, no telemetry, no crash reporting and no analytics.
It sends data over the network to two places only:

1. **The Kubernetes clusters you open**, at the API server addresses in
   your own kubeconfig. Skiff connects to a cluster because you chose it.
   Credential plugins named in your kubeconfig are started as `kubectl`
   starts them and connect wherever they connect.
2. **`api.github.com`**, to ask whether a newer release of Skiff exists:
   once at startup and every four hours while the program is open. The
   request is anonymous. It carries no identifier and nothing about you,
   your computer's contents or your clusters beyond what any HTTPS request
   reveals (your IP address) and the fixed user agent `skiff-updater`.
   This check is on by default and is turned off in Settings → Updates,
   with `"updates": {"check": false}` in `config.json`, or with the
   environment variable `SKIFF_NO_UPDATE_CHECK`. On macOS, pressing
   **Update** also downloads the new version from GitHub.

Skiff transfers nothing else to any other system. Links are opened in
your browser, not fetched by Skiff. The full account, with its limits, is
in [security.md](security.md#network-destinations).

## Installing and removing

The installers add nothing besides Skiff. Removing it is described in
[troubleshooting.md](troubleshooting.md#removing-skiff).

## Reporting a problem

- **A release file that looks wrong**: a failed checksum or attestation,
  a signature from anyone other than the one named above, or a file
  offered as Skiff from anywhere but
  [this repository's releases](https://github.com/TsenkoTsenkov/skiff/releases).
  Report it privately through
  [GitHub's vulnerability report form](https://github.com/TsenkoTsenkov/skiff/security/advisories/new);
  [SECURITY.md](../SECURITY.md) says what to include and what to expect.
- **Anything else** about a release or this policy: open an
  [issue](https://github.com/TsenkoTsenkov/skiff/issues).
