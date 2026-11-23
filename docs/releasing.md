# Releasing: the owner's checklist

Everything here is done once by the owner of the repository, in GitHub's
settings or with an account elsewhere; none of it is in the code. The
pipeline itself, each secret in detail, and the Homebrew and winget steps
are in [packaging/README.md](../packaging/README.md). This page is the
order to do things in.

Nothing below is required for a release to come out: with no secrets
configured the pipeline publishes complete, **unsigned** builds, and the
release notes say so.

## 1. Before making the repository public

- [ ] Read the README, `docs/` and `site/index.html` once as a stranger
      would. They describe unsigned builds; that stays true until step 5.
- [ ] Decide about the name. "Skiff" was also the name of an e-mail and
      documents product that was shut down in 2024, and the crates.io name
      `skiff` is taken by another project. The app's identifiers already
      avoid the bare name (`io.github.tsenkotsenkov.skiff`, winget
      `TsenkoTsenkov.Skiff`). A trademark search before paying for
      signing certificates is worth the hour; it has not been done.
- [ ] Check that no secret was ever committed: `git log -p | grep -iE
      'BEGIN (RSA|EC|OPENSSH|PRIVATE)|ghp_|AKIA'` should print nothing.
      `.gitignore` already excludes certificate and key files.
- [ ] Do not rewrite the history of `main` from here on: the build number
      is the number of commits and must only grow.

## 2. Make it public and switch features on

- [ ] Settings → General → Danger Zone → Change visibility → Public.
- [ ] Settings → General → Features: enable **Discussions** (the issue
      chooser links to it).
- [ ] Settings → Code security: enable **Private vulnerability
      reporting**. `SECURITY.md` and the issue chooser send reporters
      there; without it the link leads nowhere.
- [ ] Settings → Code security: enable Dependabot alerts (the update
      configuration is already in `.github/dependabot.yml`).
- [ ] Settings → Branches (or Rules): protect `main`: require the CI
      checks, and forbid force pushes.
- [ ] About (the gear on the repository page): description, topics, and
      the site's address as the website.

Once public, releases get build provenance attestations and the CI
minutes for macOS and Windows runners are free.

## 3. Enable the site

- [ ] Settings → Pages → Build and deployment → Source: **GitHub
      Actions**.
- [ ] Actions → Site → Run workflow (or push a change under `site/`).
      `.github/workflows/pages.yml` publishes the `site/` folder to
      `https://tsenkotsenkov.github.io/skiff/`.
- [ ] Open the page; check the download buttons after the first stable
      release exists (before that they lead to a 404, because
      `/releases/latest/download/…` needs a release marked latest).

The site makes no external requests and has no build step: what is in
`site/` is what is served.

## 4. The release environment

- [ ] Settings → Environments → New environment → `release`.
- [ ] Deployment branches and tags → Selected branches and tags: add the
      branch `main` and the tag pattern `v*`.
- [ ] Do not add required reviewers, unless every nightly should wait
      for an approval.

All secrets and variables below go into this environment, not into the
repository-wide ones. Then only a build of `main` or a `v*` tag can sign.

## 5. Signing (each part optional, any order)

Each row switches itself on when its secrets exist. The portal steps for
each are in [packaging/README.md](../packaging/README.md#secrets-and-variables).

| To get | Add to the `release` environment | Needs |
| --- | --- | --- |
| macOS app signed with a Developer ID | secrets `MACOS_CERT_P12_BASE64`, `MACOS_CERT_PASSWORD` (and `MACOS_SIGN_IDENTITY` only if the file holds several identities) | Apple Developer Program membership |
| macOS app notarized (opens without the "Open Anyway" step) | secrets `APPLE_API_KEY_P8_BASE64`, `APPLE_API_KEY_ID`, `APPLE_API_ISSUER_ID`, in addition to the row above | an App Store Connect API key |
| Windows installer and program signed, with Azure Artifact Signing | secrets `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`; variables `AZURE_SIGNING_ENDPOINT`, `AZURE_SIGNING_ACCOUNT`, `AZURE_SIGNING_PROFILE` | an Azure signing account with a validated identity |
| Windows signed with a certificate file instead | secrets `WINDOWS_CERT_PFX_BASE64`, `WINDOWS_CERT_PASSWORD` (used only when the Azure values are absent) | an exportable code-signing certificate |

Linux packages are not signed; there is nothing to configure.

After adding secrets:

- [ ] Actions → Release → Run workflow builds everything without
      publishing, but a run started by hand gets no secrets. To see
      signing work, push to `main` (a nightly) and read the "Is it
      signed?" section of that prerelease's notes. That statement is
      derived from the built files, not from which secrets exist.
- [ ] The first Developer ID build changes what installed Macs accept:
      copies of unsigned builds update to it normally; signed copies then
      refuse anything not signed by the same team.
- [ ] When builds are notarized and signed, remove the "first launch"
      notes from `README.md`, `docs/install.md` and the FAQ in
      `site/index.html`, and update the "Current limit" paragraph in
      `docs/security.md`.

## 6. Cut v0.1.0

- [ ] `main` is green in CI. The `deny` job in particular: an advisory
      published since the last run fails it.
- [ ] The version in the root `Cargo.toml` (`[workspace.package]
      version`) is `0.1.0`, and `Cargo.lock` agrees (`cargo check`).
- [ ] In `CHANGELOG.md`, rename "Unreleased" to `0.1.0` with the date and
      start a new empty "Unreleased" section. Merge that.
- [ ] Tag the merged commit and push the tag:

      ```bash
      git tag v0.1.0 && git push origin v0.1.0
      ```

      The tag must equal the version in `Cargo.toml`, or the run stops
      before building.
- [ ] Watch Actions → Release. When it finishes, the release `v0.1.0` is
      marked "Latest" and carries the eleven files and `SHA256SUMS`.
- [ ] Download one file and verify it as a user would
      ([install.md](install.md#checking-a-download)).
- [ ] Install it on each system you have. Nothing in this repository has
      been run on a real Mac or Windows machine by the release pipeline's
      authors beyond what CI does; the first release is the first real
      test of the installers and of the macOS updater.
- [ ] Open the site and click each download button.

## 7. After the release

- [ ] winget: submit the manifests
      ([packaging/README.md](../packaging/README.md#winget-windows)).
      `winget install` avoids the browser's download warning even for an
      unsigned build.
- [ ] Homebrew: only once the Mac build is signed and notarized
      ([packaging/README.md](../packaging/README.md#homebrew-macos)).
- [ ] For every later release: version in `Cargo.toml`, changelog, tag.
      Nightlies need nothing; every push to `main` that touches the code
      or the packaging publishes one, and the newest ten are kept.
