# Releasing: the owner's checklist

Everything here is done once by the owner of the repository, in GitHub's
settings or with an account elsewhere; none of it is in the code. The
pipeline itself, each secret in detail, and the Homebrew and winget steps
are in [packaging/README.md](../packaging/README.md). This page is the
order to do things in. The check of the name is [at the end](#the-name).

Nothing below is required for a release to come out: with no secrets
configured the pipeline publishes complete, **unsigned** builds, and the
release notes say so.

**Where things stand on 2026-11-24:** the repository is public and the
site is live; the macOS build is signed with a Developer ID and notarized
(seen in the release run of `build-41`); the Windows build is unsigned;
the README, the docs and the site say exactly that, and the notes about
download links that do not work yet are gone. The boxes below are not
ticked here: this page cannot see the repository's settings.

## 1. Before making the repository public

Going public cannot be undone in any useful sense: history, pull
requests, branches and Actions logs become readable and get copied. A
review of all of that was done on 2026-11-24 (every remote head, tag and
`refs/pull/*`, 46 commits). It found no token, private key, real
kubeconfig, cloud account id or Apple Team ID, in the tree or in history.
What it did find is the list below.

Decide:

- [ ] **Your e-mail address in history.** Four commits (the merge commits
      made on github.com) carry a personal address and a misspelt name;
      the others use `dev@skiff.invalid` or a bot address. Either accept
      that, or rewrite those four before the switch. A rewrite that keeps
      the number of commits does not disturb the build number. For future
      merges: github.com → Settings → Emails → **Keep my email addresses
      private**.
- [ ] **Session links in commit messages.** 38 commit messages and pull
      request bodies contain `Claude-Session:` links and co-author lines.
      They reveal nothing by themselves; keep or rewrite with the item
      above.
- [ ] **The name.** See [The name](#the-name) below. No live trademark for
      software was found, but no official register could be searched from
      where the check ran. Look at the four registers named there before
      paying for signing certificates.

Fix:

- [ ] **Font licence files.**
      `crates/skiff/assets/fonts/LICENSE-JetBrainsMono.txt` is a Debian
      packaging file that does not contain the OFL text which
      `THIRD-PARTY-NOTICES.md` says it is; `LICENSE-Inter.txt` has the OFL
      text among unrelated paragraphs. Replace both with the upstream
      `OFL.txt` of each font.
- [ ] **Notices in the packages.** `hack/linux-package.sh`,
      `hack/windows-package.ps1` and `packaging/windows/skiff.nsi` ship `LICENSE`
      only. Add `THIRD-PARTY-NOTICES.md` next to it.
- [ ] **Stale branches and pull requests.** Delete merged `claude/*`,
      `release/*` and Dependabot branches; close or merge what is open.
      They all become public.
- [ ] Commit the removal of `hack/bench/__pycache__/` (`.gitignore` now
      excludes `__pycache__/`).

Check:

- [ ] Read the README, `docs/` and `site/index.html` once as a stranger
      would. They describe which builds are signed; keep that in step
      with [step 5](#5-signing-each-part-optional-any-order).
- [ ] Build the documentation pages, `python3 hack/site-build.py`, and
      click through them (`python3 -m http.server -d site`).
- [ ] Run the secret search yourself once more: `git log -p --all | grep
      -iE 'BEGIN (RSA|EC|OPENSSH|PRIVATE)|ghp_|github_pat_|AKIA'` should
      print nothing. `.gitignore` excludes certificate and key files.
- [ ] The download links and the release badge lead nowhere until a
      stable release exists: the three releases so far are prereleases,
      and `/releases/latest` answers 404 for those. The README and the
      site no longer warn about this. Plan [step 6](#6-cut-v010) for the same day, or accept
      the gap.
- [ ] After a rewrite, if any: do not rewrite the history of `main` from
      here on. The build number is the number of commits and must only
      grow.

## 2. Make it public and switch features on

- [ ] Settings → General → Danger Zone → Change visibility → Public.
- [ ] Settings → General → Features: enable **Discussions** (the issue
      chooser links to it) and **Sponsorships**: `.github/FUNDING.yml`
      then puts a **Sponsor** button on the repository page, leading to
      Buy Me a Coffee.
- [ ] Settings → Code security: enable **Private vulnerability
      reporting**. `SECURITY.md` and the issue chooser send reporters
      there; without it the link leads nowhere.
- [ ] Settings → Code security: enable **Secret scanning** with **push
      protection**, and Dependabot alerts (the update configuration is
      already in `.github/dependabot.yml`).
- [ ] Settings → Branches (or Rules): protect `main`: require the CI
      checks, forbid force pushes and deletion.
- [ ] Settings → Actions → General: workflow permissions **Read
      repository contents**; do not allow Actions to create or approve
      pull requests; require approval for workflows from outside
      contributors' forks.
- [ ] About (the gear on the repository page): description, topics, and
      the site's address as the website.

Once public, releases get build provenance attestations and the CI
minutes for macOS and Windows runners are free.

## 3. Enable the site

- [ ] Settings → Pages → Build and deployment → Source: **GitHub
      Actions**.
- [ ] Actions → Site → Run workflow (or push a change under `site/` or
      `docs/`). `.github/workflows/pages.yml` builds the documentation
      pages from `docs/*.md` with `hack/site-build.py`, then publishes the
      `site/` folder to `https://tsenkotsenkov.github.io/skiff/`.
- [ ] Open the page; check the download buttons after the first stable
      release exists (before that they lead to a 404, because
      `/releases/latest/download/…` needs a release marked latest).

The site makes no external requests; the workflow checks that on every
run. The landing page is served as it is in `site/`. The pages under
`/docs/` are generated in the workflow and are not committed. The
"Support" buttons are plain links to Buy Me a Coffee: nothing is loaded
from there.

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

**Windows through the SignPath Foundation.** The foundation signs
open-source projects' Windows binaries free of charge, and Skiff is
applying. The Windows job of `release.yml` has the steps for it, but they
are skipped until the secret `SIGNPATH_API_TOKEN` and the three
`SIGNPATH_*` variables exist, and they have never run
([packaging/README.md](../packaging/README.md#secrets-and-variables)).
Nothing is signed by SignPath today. Its conditions, as
understood here (read the current ones on signpath.org before applying):
an OSI licence, a "Code signing policy" page linked from the home page
that names committers, reviewers and approvers and states the privacy
policy ([code-signing-policy.md](code-signing-policy.md), linked from the
README and the site's download section), binaries built from the public
source by a verifiable CI, and two-factor authentication on the accounts
involved.

- [ ] Confirm two-factor authentication is on for the owner's GitHub
      account; the policy states it as a rule.
- [ ] Apply at signpath.org with the policy page's address.
- [ ] If accepted: add the four `SIGNPATH_*` values to the `release`
      environment as packaging/README.md describes, check that a nightly
      comes out signed, then in the policy move the "once approved" note
      into the table, and update the Windows statements in `README.md`,
      `docs/install.md`, `docs/troubleshooting.md`, `docs/security.md`
      and `site/index.html`.

After adding secrets:

- [ ] Actions → Release → Run workflow builds everything without
      publishing, but a run started by hand gets no secrets. To see
      signing work, push to `main` (a nightly) and read the "Is it
      signed?" section of that prerelease's notes. That statement is
      derived from the built files, not from which secrets exist.
- [ ] The first Developer ID build changes what installed Macs accept:
      copies of unsigned builds update to it normally; signed copies then
      refuse anything not signed by the same team.
- [x] macOS: the "first launch" notes are removed from `README.md`,
      `docs/install.md`, `docs/troubleshooting.md` and the FAQ in
      `site/index.html`, and `docs/security.md` is updated. The Windows
      notes stay until the Windows build is signed.

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
- [x] The "until the first stable release" notes are removed from the
      Install section of `README.md` and the Download section of
      `site/index.html`.

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

## The name

A quick check of the name "Skiff" was made on 2026-11-24. It is not legal
advice, and it is not a register search: the official trademark registers
(USPTO TSDR, EUIPO, WIPO Global Brand Database, UK IPO) could not be
reached from where the check ran. The trademark lines below come from
search-result summaries of aggregator pages, which were not opened; the
package-registry and GitHub lines were read directly.

**Trademarks found for SKIFF (United States)**

| Owner | Serial | Classes | Status as listed | Source |
| --- | --- | --- | --- | --- |
| Skiff World, Inc. (the e-mail and documents product) | 90887371 | 42 | filed 2021-08-17; dead, never registered | <https://markinton.com/trademark/skiff-90887371> |
| Corsair Memory, Inc. | 99185738 | 9, 28 | filed 2025-05-14; pending. Goods listed: computer keyboards, not software | <https://markinton.com/trademark/skiff-99185738> |
| Templeton Ventures | 86414237 | 9, 42 | abandoned | <https://trademarks.justia.com/864/14/skiff-86414237.html> |
| Alesia Holdings | 77786904 | 9 | dead | <https://markinton.com/trademark/skiff-77786904> |

No live registration or application for SKIFF covering software was
found. Notion bought Skiff World's product in February 2024 and shut it
down that year (<https://en.wikipedia.org/wiki/Skiff_(email_service)>); no
assignment of the mark to Notion was found. Nothing surfaced for the EU,
the UK or WIPO, which says little, because those registers were not
searched.

**Other software called Skiff**

- Package names that are taken: crates.io `skiff` (a scripting language,
  last updated 2021), npm `skiff` (Raft for Node.js), PyPI `skiff` (a
  DigitalOcean library), and winget `Skiff.Skiff` (Skiff World's desktop
  app; from a search result). Homebrew has neither a cask nor a formula
  named `skiff`.
- On GitHub: `skiff-org/skiff-apps`, `skiffos/SkiffOS`,
  `basecamp/kamal-skiff`, and several small desktop tools, among them a
  disk-cleanup tool and an SSH manager. In the Kubernetes space only two
  projects with a handful of stars.
- A Mac App Store listing "Skiff Desktop" by Skiff World existed; whether
  it is still live was not confirmed.

**What follows from it**

- The app's identifiers already avoid the bare name:
  `io.github.tsenkotsenkov.skiff`, winget `TsenkoTsenkov.Skiff`. Keep it
  that way; do not publish a crate named `skiff`.
- The open question is Corsair's pending class 9 application. Keyboards
  are far from a Kubernetes client, but the wording of the application
  has to be read on TSDR, not on an aggregator.
- Notion may hold unregistered rights or registrations outside the US.

**Register check, 2026-11-24** (USPTO TSDR and trademark search; TMview,
which covers EUIPO, the UK IPO and the national offices; WIPO Global
Brand Database). Still not legal advice.

- US: Corsair's 99185738 is now **registered** (2026-10-20), for
  computer keyboards and gaming keyboards only. 90887371 (Skiff World) is
  dead, abandoned 2023. No other live SKIFF mark in class 9 or 42.
- **EU: EUIPO 008131807, word mark SKIFF, registered, owner NC
  Transaction, Inc. (News Corp, from its 2009 Skiff e-reader).** Class 9
  includes "computer software in the field of text, image and sound
  transmission, manipulation and display". Renewed to 2029-03-02; the
  file was last updated in July 2026. The same owner holds UK
  UK00908131807 and UK00002509584 (classes 9, 35, 38), China 7190553,
  and Skiff LLC holds Italy and Japan registrations. The e-reader was
  discontinued in 2010, so these may be open to revocation for non-use,
  but they are on the register.
- Brazil: 501655133, Skiff, class 42, registered, Nightwatch, Inc. (from
  Skiff World's international registration 1655133, which has ended).
- Similar, not identical, in the EU: SKIFFER (Tutus Data AB, EUIPO
  018370988, classes 9, 38, 42).

The owner decided to launch as Skiff with this known. If NC Transaction
objects, the rename candidates below are the fallback.

Still to do:

- [x] Look up serials 99185738 and 90887371 on TSDR.
- [x] Search SKIFF in classes 9 and 42 on EUIPO, WIPO and the UK IPO.
- [ ] Look at `manifests/s/Skiff` in `microsoft/winget-pkgs`, and at
      WHOIS for any domain you want.
- [ ] If the name has to change: Shallop and Umiak were free on
      crates.io, npm, PyPI and Homebrew and had no notable GitHub
      project on the same day; Pinnace, Sampan and Catboat were free on
      crates.io and PyPI but taken on npm. Dory, Wherry, Skerry and Proa
      were rejected: each collides with an existing tool, two of them
      Kubernetes tools. None of these was checked for trademarks.
