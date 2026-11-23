# Packaging and releasing Skiff

This directory holds what turns the `skiff` binary into something people can
install, and this file says how a release is made and what the owner of the
repository has to set up for signed builds.

| Path | What it is |
|---|---|
| `macos/Info.plist.in`, `macos/Skiff.entitlements` | The app bundle's property list (template) and the entitlements of the signed build |
| `linux/io.github.tsenkotsenkov.skiff.desktop`, `…metainfo.xml` | Desktop entry and AppStream metadata |
| `windows/skiff.nsi` | The per-user installer (NSIS) |
| `homebrew/skiff.rb` | Homebrew cask template |
| `winget/*.yaml` | winget manifest templates |

The scripts that use them are in `hack/`:

| Script | Runs on | Makes |
|---|---|---|
| `hack/macos-app.sh --universal --dmg --zip` | a Mac | `Skiff.app`, `Skiff-macos-universal.dmg`, the updater zips |
| `hack/linux-package.sh` | Linux | `dist/skiff-linux-<arch>.tar.gz`, `dist/skiff_<amd64\|arm64>.deb`, `dist/Skiff-linux-<arch>.AppImage` |
| `hack/windows-package.ps1` | Windows | `dist\Skiff-windows-x64.zip`, `dist\Skiff-windows-x64-setup.exe` |

Each works on a developer's machine with no secrets at all and then makes
unsigned packages; the header of each script lists its options.

## How a release happens

`.github/workflows/release.yml` does everything.

- **Stable release.** Set the version in the root `Cargo.toml`
  (`[workspace.package] version`), run `cargo check` so that `Cargo.lock`
  follows, merge that, then tag the merged commit:

  ```bash
  git tag v1.2.3 && git push origin v1.2.3
  ```

  The tag must equal the version in `Cargo.toml`, or the run stops before
  building. The release is published as `v1.2.3` and marked "Latest".
- **Nightly.** Every push to `main` that touches the code or the packaging
  publishes the prerelease `build-<N>`. The newest 10 are kept. People get
  these only if they set `"updates": {"channel": "nightly"}` in Skiff's
  config.
- **Trying the pipeline.** Actions → Release → Run workflow (or push a branch
  named `release/…`) builds everything and attaches it to the run as
  artifacts. Nothing is published.

`<N>` is the build number, `git rev-list --count HEAD`: the number of commits
on the built revision. It is compiled into the program and is what the
in-app updater compares, so it must only ever grow. Do not rewrite the
history of `main`.

Every release has the same file names, so that
`https://github.com/TsenkoTsenkov/skiff/releases/latest/download/<name>`
always points at the current stable one:

```
Skiff-macos-universal.dmg
Skiff-macos-arm64-build-<N>.zip      (for the in-app updater)
Skiff-macos-x86_64-build-<N>.zip     (the same app, for Intel Macs' updater)
Skiff-windows-x64-setup.exe
Skiff-windows-x64.zip
Skiff-linux-x86_64.AppImage          Skiff-linux-aarch64.AppImage
skiff-linux-x86_64.tar.gz            skiff-linux-aarch64.tar.gz
skiff_amd64.deb                      skiff_arm64.deb
SHA256SUMS
```

The release notes state for macOS and Windows whether that build is signed.
That statement is derived from the built files themselves (`stapler
validate`, `codesign`, `Get-AuthenticodeSignature`), not from which secrets
were configured.

### Verifying a download

```bash
sha256sum -c SHA256SUMS --ignore-missing
gh attestation verify Skiff-linux-x86_64.AppImage -R TsenkoTsenkov/skiff
```

The second command checks the build provenance attestation: proof, signed
by GitHub, that the file was built by this repository's release workflow.
GitHub offers attestations for public repositories only; while the
repository is private the workflow skips that step and the release notes say
so.

## One-time setup in the repository

1. **Settings → Environments → New environment → `release`.** The workflow
   uses this environment for the jobs of a build that will be published.
   Under "Deployment branches and tags" choose "Selected branches and tags"
   and add the branch `main` and the tag pattern `v*`. Do not add required
   reviewers unless every nightly should wait for an approval.
2. **Put all secrets and variables below into that environment**
   (Environment secrets / Environment variables), not into the
   repository-wide ones. Then only a build of `main` or of a `v*` tag can
   sign anything; a run started by hand or from another branch gets no
   secrets and builds unsigned.
3. **Settings → Actions → General → Workflow permissions:** "Read
   repository contents" is enough; the workflow asks for more only in its
   publish job.
4. Optional: Settings → General → Features → Discussions (the issue chooser
   links there), and Settings → Code security → Private vulnerability
   reporting (the issue chooser links there too).

With nothing configured the pipeline still publishes complete, unsigned
releases.

## Secrets and variables

| Name | Kind | What it enables |
|---|---|---|
| `MACOS_CERT_P12_BASE64` | secret | Signing the Mac app with a Developer ID. Without it the app is signed ad hoc and Gatekeeper refuses a downloaded copy until the user allows it. |
| `MACOS_CERT_PASSWORD` | secret | The password of that `.p12` file. |
| `MACOS_SIGN_IDENTITY` | secret, optional | The identity to sign with, `Developer ID Application: Name (TEAMID)`. Only needed if the `.p12` holds more than one; otherwise it is read from the certificate. |
| `APPLE_API_KEY_P8_BASE64` | secret | Notarization by Apple (with the two below). Without the three the app is signed but not notarized, and Gatekeeper still refuses it. |
| `APPLE_API_KEY_ID` | secret | The key's ID (10 characters). |
| `APPLE_API_ISSUER_ID` | secret | The issuer ID of the App Store Connect team (a UUID). |
| `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET` | secrets | Signing the Windows program and installer with Azure Artifact Signing: the service principal that may sign. |
| `AZURE_SIGNING_ENDPOINT`, `AZURE_SIGNING_ACCOUNT`, `AZURE_SIGNING_PROFILE` | variables | The signing account's regional endpoint (for example `https://weu.codesigning.azure.net`), its name, and the certificate profile's name. |
| `WINDOWS_CERT_PFX_BASE64`, `WINDOWS_CERT_PASSWORD` | secrets | The alternative for Windows: a code-signing certificate as a `.pfx` file. Used only if the Azure values are not set. |

The Apple Team ID is not a setting of its own: it is taken from the signing
identity and compiled into the app as `SKIFF_TEAM_ID`. A build that carries
it accepts updates only if they are signed by that same team; an unsigned
build carries none.

Base64 without line breaks: `base64 -i file | tr -d '\n' | pbcopy` on a Mac,
`base64 -w0 file` on Linux.

### macOS: Developer ID certificate

Needs membership of the Apple Developer Program (a yearly fee; 99 USD at
the time of writing, check Apple's site). Only the Account Holder of the
membership can create this kind of certificate.

1. On a Mac open Keychain Access → menu Keychain Access → Certificate
   Assistant → Request a Certificate From a Certificate Authority. Enter
   your e-mail address and name, choose "Saved to disk". This writes a
   `.certSigningRequest` file and keeps the private key in your keychain.
2. At <https://developer.apple.com/account/resources/certificates/add>
   choose **Developer ID Application**, upload the request file and download
   the `.cer`. Double-click it: it joins its private key in Keychain Access.
3. In Keychain Access → login → My Certificates, select "Developer ID
   Application: …", right-click → Export, format "Personal Information
   Exchange (.p12)", and give it a strong password.
4. `MACOS_CERT_P12_BASE64` = the base64 of that file; `MACOS_CERT_PASSWORD`
   = the password. Delete the exported file afterwards.

To see the identity's exact name: `security find-identity -v -p codesigning`.

The certificate is valid for five years. Apps signed while it was valid keep
working after that; new builds need a new certificate. The Team ID stays the
same when it is renewed, so the updater keeps accepting new builds.

### macOS: App Store Connect API key (for notarization)

1. <https://appstoreconnect.apple.com/access/integrations/api> (Users and
   Access → Integrations → App Store Connect API → Team Keys). The first time,
   the Account Holder has to request access to the API there.
2. Generate a key with the role **Developer**. Download the `.p8` file: it
   can be downloaded exactly once.
3. `APPLE_API_KEY_P8_BASE64` = the base64 of the `.p8` file,
   `APPLE_API_KEY_ID` = the Key ID shown in the list,
   `APPLE_API_ISSUER_ID` = the Issuer ID shown above the list.

An API key is used rather than an Apple ID with an app-specific password
because it is not tied to a person's login and can be revoked on its own.

To try signing and notarizing on your own Mac before trusting the workflow:

```bash
MACOS_SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" \
APPLE_API_KEY_PATH=~/AuthKey_XXXXXXXXXX.p8 APPLE_API_KEY_ID=XXXXXXXXXX \
APPLE_API_ISSUER_ID=xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx \
  hack/macos-app.sh --universal --dmg --zip
spctl --assess --type execute -vv target/release/Skiff.app   # "Notarized Developer ID"
```

The first signed release changes what installed copies accept: copies
installed from unsigned builds update to it normally; from then on, signed
copies refuse anything not signed by the same team.

### Windows: which way to sign

Unsigned builds work, but Windows shows "Windows protected your PC"
(SmartScreen) and the user has to choose More info → Run anyway. There are
three ways out; none is both free and immediate.

**SignPath Foundation** (free for open-source projects). Requirements, as
published at <https://signpath.org/terms>: an OSI-approved licence (MIT
qualifies), a public repository, releases built by CI, multi-factor
authentication for the maintainers, and a code signing policy page in the
repository. The certificate's publisher name is "SignPath Foundation".

1. Make the repository public and publish at least one release.
2. Add a "Code signing policy" section to the README or a
   `CODE_SIGNING_POLICY.md` (their site has the wording they expect: who
   may commit, who approves releases, a privacy statement).
3. Apply at <https://signpath.org/apply>. Approval is at their discretion
   and takes a while.
4. They give you an organization ID, a project slug and an API token.
   SignPath signs artifacts that were uploaded by a GitHub Actions run, so
   this needs a change to the workflow rather than just secrets: after the
   `windows` job's upload, call their `signpath/github-action-submit-signing-request`
   action with the artifact's ID and replace the artifact with what it
   returns. The installer contains `skiff.exe`, so their artifact
   configuration has to sign the inner file too, or the job is split in two
   (sign `skiff.exe`, build the installer from the signed file, sign the
   installer). This is not wired up yet, because the details depend on the
   project SignPath sets up for you.

**Azure Artifact Signing** (formerly Trusted Signing; about 10 USD a month at
the time of writing). Wired up: set the six `AZURE_*` values. Whether an
individual (rather than a registered organization) can pass identity
validation depends on the country; check in the Azure portal before paying.

1. In the Azure portal create an "Artifact Signing account" (pick a region;
   its endpoint, such as `https://weu.codesigning.azure.net`, is
   `AZURE_SIGNING_ENDPOINT`, its name `AZURE_SIGNING_ACCOUNT`).
2. In the account: Identity validation → New → complete it (documents are
   checked; this can take days).
3. Certificate profiles → Create → Public Trust, using that identity. Its
   name is `AZURE_SIGNING_PROFILE`.
4. Microsoft Entra ID → App registrations → New registration. Its
   Application (client) ID is `AZURE_CLIENT_ID`, the Directory (tenant) ID
   `AZURE_TENANT_ID`. Under Certificates & secrets create a client secret:
   `AZURE_CLIENT_SECRET` (it expires; renew it before it does).
5. In the signing account → Access control (IAM) → Add role assignment →
   "Artifact Signing Certificate Profile Signer" (older name: "Trusted
   Signing Certificate Profile Signer") → assign it to the app registration.

`hack/windows-package.ps1` then signs `skiff.exe` before packing it and the
installer after making it, using Microsoft's `TrustedSigning` PowerShell
module, and fails the build if a file is not validly signed afterwards.

**A certificate from a certificate authority** (a few hundred USD a year).
Since 2023 the private keys of such certificates must live on a hardware
token or in a cloud HSM, so most cannot be exported as a file. If yours can
(or for testing with a self-signed one), set `WINDOWS_CERT_PFX_BASE64` and
`WINDOWS_CERT_PASSWORD`; the script signs with `signtool`.

Also worth doing regardless: publish to winget (below). `winget install`
does not show the browser's download warning.

### Linux

Nothing to configure. Linux packages are not signed; `SHA256SUMS` and the
attestation are how a download is verified. The binaries are built on
Ubuntu 22.04 and need glibc 2.35 or newer. No GPU, Wayland or X11 library
is bundled, in the AppImage either: the system's own are loaded at run
time.

## Homebrew (macOS)

Homebrew is turning away apps that fail Gatekeeper, so do this once the Mac
build is signed and notarized.

1. Create the public repository `TsenkoTsenkov/homebrew-tap`.
2. Copy `packaging/homebrew/skiff.rb` to `Casks/skiff.rb` in it.
3. For each stable release set `version` and `sha256`. The hash is in the
   release's checksums:

   ```bash
   curl -sL https://github.com/TsenkoTsenkov/skiff/releases/download/v1.2.3/SHA256SUMS | grep universal.dmg
   ```

4. Check and publish:

   ```bash
   brew tap tsenkotsenkov/tap && brew audit --cask --online skiff
   brew install --cask tsenkotsenkov/tap/skiff
   ```

Users then install with `brew install --cask tsenkotsenkov/tap/skiff`. The
cask declares `auto_updates`, because the app updates itself.

Getting into Homebrew's own cask repository (so that `brew install --cask
skiff` works without the tap) additionally asks for a certain popularity of
the project; try once it has users.

## winget (Windows)

1. For a stable release, copy the three files in `packaging/winget/` and
   replace the version (`0.1.0`) everywhere, `ReleaseDate`, and
   `InstallerSha256` with the installer's hash from `SHA256SUMS` in upper
   case.
2. Validate and try them on a Windows machine:

   ```powershell
   winget validate --manifest .\manifests
   winget install --manifest .\manifests     # needs: winget settings --enable LocalManifestFiles
   ```

3. Fork <https://github.com/microsoft/winget-pkgs>, put the files under
   `manifests/t/TsenkoTsenkov/Skiff/<version>/` and open a pull request.
   Microsoft's bots install the package in a sandbox; a person approves
   the first submission. `wingetcreate update TsenkoTsenkov.Skiff --version
   1.2.3 --urls <installer url> --submit` does all of this for later
   versions.

winget requires version-numbered releases, which is why only `v*` tags are
submitted, never nightlies.

## Updating the pinned tools

- **Rust:** `rust-toolchain.toml` (everything builds with exactly that
  version). `rust-version` in `Cargo.toml` is the oldest version that is
  expected to work.
- **GitHub Actions:** pinned by commit in the workflows; Dependabot proposes
  updates.
- **appimagetool:** version and SHA-256 at the top of
  `hack/linux-package.sh`.
- **cargo-deny:** version and SHA-256 in `.github/workflows/ci.yml`; its
  rules are in `deny.toml`.
