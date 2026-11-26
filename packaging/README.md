# Packaging and releasing Skiff

This directory holds what turns the `skiff` binary into something people can
install, and this file says how a release is made and what the owner of the
repository has to set up for signed builds.

| Path | What it is |
|---|---|
| `macos/Info.plist.in`, `macos/Skiff.entitlements` | The app bundle's property list (template) and the entitlements of the signed build |
| `linux/io.github.tsenkotsenkov.skiff.desktop`, `…metainfo.xml` | Desktop entry and AppStream metadata |
| `windows/skiff.nsi` | The per-user installer (NSIS) |
| `winget/*.yaml` | winget manifest templates |
| `aur/PKGBUILD.in` | AUR package template (`skiff-bin`) |
| `../Casks/skiff.rb` | Homebrew cask: this repository is the tap |
| `../bucket/skiff.json` | Scoop manifest: this repository is the bucket |

The scripts that use them are in `hack/`:

| Script | Runs on | Makes |
|---|---|---|
| `hack/macos-app.sh --universal --dmg --zip` | a Mac | `Skiff.app`, `Skiff-macos-universal.dmg`, the updater zips |
| `hack/macos-app.sh --target aarch64-apple-darwin --dmg` | a Mac | `Skiff.app` for Apple Silicon only, `Skiff-macos-arm64.dmg` |
| `hack/linux-package.sh` | Linux | `dist/skiff-linux-<arch>.tar.gz`, `dist/skiff_<amd64\|arm64>.deb`, `dist/Skiff-linux-<arch>.AppImage` |
| `hack/windows-package.ps1` | Windows | `dist\Skiff-windows-x64.zip`, `dist\Skiff-windows-x64-setup.exe` |

Each works on a developer's machine with no secrets at all and then makes
unsigned packages; the header of each script lists its options.

## How a release happens

`.github/workflows/release.yml` does everything.

- **Stable release (the normal way).** The pull request bumps the
  version with `hack/bump-version.py` (patch by default; `minor`, `major`
  or an exact `X.Y.Z` also work). It changes `Cargo.toml`, `Cargo.lock`
  and `CHANGELOG.md`, whose "Unreleased" entries become the new version's
  section. When that is merged, the push to `main` finds a version that
  has no `vX.Y.Z` tag yet, publishes it as the stable release `vX.Y.Z`,
  marked "Latest", and creates the tag on that commit.
- **Nightly (experimental).** A push to `main` whose version is already
  released publishes the prerelease `build-<N>` instead. So a change that
  is merged *without* a version bump reaches only people on the nightly
  channel (`"updates": {"channel": "nightly"}` in Skiff's config); use
  that for changes that could break things badly and should be tried
  first. The newest 10 nightlies are kept. The next version bump ships
  everything merged since the last release.
- **Stable release by hand.** Pushing a tag `vX.Y.Z` also publishes a
  stable release. The tag must equal the version in `Cargo.toml`, or the
  run stops before building.
- **Trying the pipeline.** Actions → Release → Run workflow (or push a branch
  named `release/…`) builds everything and attaches it to the run as
  artifacts. Nothing is published.
- **Pull requests.** A pull request that changes the release workflow, one
  of the three packaging scripts, anything under `packaging/`, or the
  program's build script or icons runs the same builds and checks: every
  package is made, installed or started, and attached to the run. Such a
  run can only read the repository, is given no secret (so nothing is
  signed) and has no publish job.

`<N>` is the build number, `git rev-list --count HEAD`: the number of commits
on the built revision. It is compiled into the program and is what the
in-app updater compares, so it must only ever grow. Do not rewrite the
history of `main`.

Every release has the same file names, so that
`https://github.com/TsenkoTsenkov/skiff/releases/latest/download/<name>`
always points at the current stable one:

```
Skiff-macos-universal.dmg
Skiff-macos-arm64.dmg                (the Apple Silicon half on its own)
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
   sign anything; a run started by hand, from another branch or by a pull
   request gets no secrets and builds unsigned. Environment secrets only:
   a secret stored for the whole repository can be read by any workflow
   on any branch of it.
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
| `APPLE_TEAM_ID` | variable | Required together with `MACOS_CERT_P12_BASE64`: the Team ID of that certificate (10 characters, the part in brackets of `Developer ID Application: Name (TEAMID)`). It is compiled into the app, which then only accepts updates signed by that team. Not a secret. The program is compiled before the certificate is put on the build machine, which is why the ID cannot be read from the certificate; the build fails if the two do not match, or if the certificate is set and this is not. |
| `MACOS_SIGN_IDENTITY` | secret, optional | The identity to sign with, `Developer ID Application: Name (TEAMID)`. Only needed if the `.p12` holds more than one; otherwise it is read from the certificate. |
| `APPLE_API_KEY_P8_BASE64` | secret | Notarization by Apple (with the two below). Without the three the app is signed but not notarized, and Gatekeeper still refuses it. |
| `APPLE_API_KEY_ID` | secret | The key's ID (10 characters). |
| `APPLE_API_ISSUER_ID` | secret | The issuer ID of the App Store Connect team (a UUID). |
| `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET` | secrets | Signing the Windows program and installer with Azure Artifact Signing: the service principal that may sign. |
| `AZURE_SIGNING_ENDPOINT`, `AZURE_SIGNING_ACCOUNT`, `AZURE_SIGNING_PROFILE` | variables | The signing account's regional endpoint (for example `https://weu.codesigning.azure.net`), its name, and the certificate profile's name. |
| `WINDOWS_CERT_PFX_BASE64`, `WINDOWS_CERT_PASSWORD` | secrets | The alternative for Windows: a code-signing certificate as a `.pfx` file. Used only if the Azure values are not set. |
| `SIGNPATH_API_TOKEN` | secret | Signing the Windows program and installer through SignPath: the API token of the SignPath user that submits the signing requests. Its presence switches the SignPath route on. |
| `SIGNPATH_ORGANIZATION_ID`, `SIGNPATH_PROJECT_SLUG`, `SIGNPATH_SIGNING_POLICY_SLUG` | variables | Required together with `SIGNPATH_API_TOKEN`: the SignPath organization's ID, and the slugs of the project and of the signing policy to sign with. Not secrets. |

Set up at most one of the three Windows routes. The Windows job stops
before building if `SIGNPATH_API_TOKEN` is set together with
`AZURE_CLIENT_SECRET` or `WINDOWS_CERT_PFX_BASE64`, and also if only some of
the four `SIGNPATH_*` values are set.

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
5. Set the variable `APPLE_TEAM_ID` (an environment variable of `release`,
   not a secret) to the certificate's Team ID: the 10 characters in
   brackets at the end of the identity's name.

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

**SignPath Foundation** (free for open-source projects). Wired up, but
never run: it cannot be tried before SignPath has set up an account for the
project, so expect to adjust details on the first build that uses it. The
Windows builds stay unsigned until the four `SIGNPATH_*` values exist.
Requirements, as published at <https://signpath.org/terms> (read the current
ones before applying): an OSI-approved licence (MIT qualifies), a public
repository, releases built by CI, multi-factor authentication for the
maintainers, and a code signing policy page in the repository. The
certificate's publisher name is "SignPath Foundation", not the owner's.

What the owner does:

1. Publish the first stable release (tag `v0.1.0`), unsigned on Windows. The
   repository is public and `docs/code-signing-policy.md` is the policy page.
2. Apply at <https://signpath.org/apply>. Approval is at their discretion
   and takes a while. When they accept, there is a SignPath organization
   for the project; its ID is shown in the SignPath web application.
3. In SignPath, create (or check, if they created it) the **project**:
   - Repository URL `https://github.com/TsenkoTsenkov/skiff`.
   - Trusted build system: the predefined "GitHub.com", added to the
     organization and linked to the project.
4. In the project, make this the default **artifact configuration**. GitHub
   stores an artifact as a ZIP archive, so the root is a `<zip-file>`; each
   request holds one of the two files, hence `min-matches="0"`:

   ```xml
   <artifact-configuration xmlns="http://signpath.io/artifact-configuration/v1">
     <zip-file>
       <pe-file path="skiff.exe" min-matches="0">
         <authenticode-sign/>
       </pe-file>
       <pe-file path="Skiff-windows-x64-setup.exe" min-matches="0">
         <authenticode-sign/>
       </pe-file>
     </zip-file>
   </artifact-configuration>
   ```

   The workflow names no artifact configuration, so the project's default
   one is used.
5. The **signing policy**: the one for release signing (with the
   certificate Windows trusts; a test-signing policy's certificate is not
   trusted on the build machine and the job then fails). If it verifies the
   origin, it must allow builds of the branch `main` and of `v*` tags. Add
   as Submitter the user whose token the workflow uses (a CI user made for
   this is what SignPath recommends), and generate that user's API token:
   it is shown once.
6. In the repository's `release` environment set the secret
   `SIGNPATH_API_TOKEN` and the variables `SIGNPATH_ORGANIZATION_ID`,
   `SIGNPATH_PROJECT_SLUG` and `SIGNPATH_SIGNING_POLICY_SLUG`. Set all four
   before the next push to `main`: a build that finds only some of them
   fails on purpose. Remove the Azure and certificate-file secrets if they
   were ever set.
7. Push to `main` and watch the nightly's Windows job. Its last check
   prints the signature status of both files; the release notes say
   "Signed" only if both are validly signed. Then update the Windows
   statements listed in `docs/releasing.md`.

What the Windows job then does. SignPath only signs a file that a run of
the workflow has stored as an artifact of the run, and it cannot open an
NSIS installer to sign what is inside. So there are two signing requests,
each made with SignPath's action
(`signpath/github-action-submit-signing-request`, pinned by commit), which
waits for the result and downloads it:

1. `skiff.exe` is compiled as always, in a step that has no secret.
2. It is stored as the artifact `unsigned-skiff-exe` and sent to SignPath.
3. `hack/windows-package.ps1 -Exe <signed skiff.exe> -RequireSignedExe`
   makes the portable zip and the installer from the signed file, and stops
   if that file's signature is not valid.
4. The installer is stored as `unsigned-skiff-setup` and sent to SignPath.
5. The signed installer replaces the unsigned one in `dist\`, after its
   signature was checked. The checks, checksums and the release use only
   what is in `dist\`.

The two `unsigned-…` artifacts are kept with the run for a day; the publish
job only gathers artifacts named `skiff-*`, so they never reach a release.
As with the other routes, the uninstaller inside the installer is not
signed.

Things to know:

- The action waits 10 minutes for each request. If the signing policy asks
  for a manual approval, somebody has to approve two requests per build
  within that time, for every nightly too; otherwise raise
  `wait-for-completion-timeout-in-seconds` on the two steps (the job's own
  limit is 60 minutes).
- The token is given only to the two steps that run SignPath's action,
  after the compiler has finished, and only in a build that will be
  published. A pull request or a run started by hand never has it.
- SignPath's documentation says the job's GitHub token needs to read
  Actions data only when the repository is private and their GitHub App is
  not installed. The workflow grants `contents: read` only; if the first
  request fails for lack of permission, install the SignPath GitHub App on
  the repository.

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
token or in a cloud HSM, so most cannot be exported as a file. If yours can,
set `WINDOWS_CERT_PFX_BASE64` and `WINDOWS_CERT_PASSWORD`; the script signs
with `signtool`. A self-signed certificate does not work here, not for a
trial either: the script requires a signature that Windows trusts, and a
self-signed one is not trusted on the build machine.

Also worth doing regardless: publish to winget (below). `winget install`
does not show the browser's download warning.

### Linux

Nothing to configure. Linux packages are not signed; `SHA256SUMS` and the
attestation are how a download is verified. The binaries are built on
Ubuntu 22.04 and need glibc 2.35 or newer. No GPU, Wayland or X11 library
is bundled, in the AppImage either: the system's own are loaded at run
time.

## Package managers

| Manager | Install command | Kept current by | Needs |
|---|---|---|---|
| Homebrew | `brew install --cask tsenkotsenkov/skiff/skiff` | `hack/bump-version.py` | nothing |
| Scoop | `scoop bucket add skiff https://github.com/TsenkoTsenkov/skiff` then `scoop install skiff/skiff` | `hack/bump-version.py` | nothing |
| winget | `winget install TsenkoTsenkov.Skiff` | `.github/workflows/package-managers.yml` | the secret `WINGET_TOKEN` |
| AUR | `yay -S skiff-bin` (or any AUR helper) | `.github/workflows/package-managers.yml` | the secret `AUR_SSH_PRIVATE_KEY` |

### Homebrew and Scoop

This repository is its own Homebrew tap (`Casks/skiff.rb`) and Scoop bucket
(`bucket/skiff.json`), so there is no second repository to keep in step.
Neither file carries a checksum: the hashes only exist once the release
workflow has built the files, and a commit to `main` after that would start
another build. `hack/bump-version.py` sets the version in both, so a version
bump is all a release needs. For about half an hour after a bump is merged,
until the release is published, they name a download that does not exist
yet; `brew` and `scoop` then fail with a 404 and work on the next try.
The Mac download is signed and notarized and Gatekeeper checks it; every
file's hash is in the release's `SHA256SUMS`.

Getting into Homebrew's own cask repository (so that `brew install --cask
skiff` works without the tap) needs a fixed checksum per version and a
certain popularity of the project, and the name `skiff` may be taken there;
try once the project has users.

### winget and the AUR

Both need a copy of each release's manifest, with its hashes, in someone
else's repository. `.github/workflows/package-managers.yml` makes them with
`hack/package-manifests.py` once the Release workflow has published a
stable version, keeps them with the run as the artifact `package-manifests`,
and submits them if the secret for that manager exists in the `release`
environment. Without the secret that job says so and does nothing.
After adding a secret, submit the current release with Actions → Package
managers → Run workflow.

**winget** (`WINGET_TOKEN`)

1. On GitHub, Settings → Developer settings → Personal access tokens →
   Tokens (classic) → Generate new token (classic), scope **public_repo**
   only. (wingetcreate needs a classic token: it forks
   `microsoft/winget-pkgs` into your account and opens the pull request
   from there.) Give it an expiry and renew it.
2. Store it as the secret `WINGET_TOKEN` of the `release` environment.

Microsoft's bots install the package in a sandbox; a person at Microsoft
approves the first version of a new package, later versions are merged
automatically when the checks pass. Unsigned installers are accepted, and
`winget install` does not show the browser's SmartScreen warning.

To try the manifests on Windows by hand: download the artifact, then
`winget validate --manifest .\winget` and
`winget install --manifest .\winget` (after
`winget settings --enable LocalManifestFiles`).

**AUR** (`AUR_SSH_PRIVATE_KEY`)

1. Create an account at <https://aur.archlinux.org/register>.
2. Make a key for it alone: `ssh-keygen -t ed25519 -f aur -C skiff-aur -N ""`.
   Paste `aur.pub` into "SSH Public Key" of the account (My Account).
3. Store the content of the file `aur` (the private key) as the secret
   `AUR_SSH_PRIVATE_KEY` of the `release` environment, and delete the file.

The first push creates the package `skiff-bin`, owned by that account.

## Updating the pinned tools

- **Rust:** `rust-toolchain.toml` (everything builds with exactly that
  version). `rust-version` in `Cargo.toml` is the oldest version that is
  expected to work.
- **GitHub Actions:** pinned by commit in the workflows; Dependabot proposes
  updates.
- **appimagetool and the AppImage runtime:** version and SHA-256 of each,
  for both architectures, at the top of `hack/linux-package.sh`. The runtime
  is a dated release of <https://github.com/AppImage/type2-runtime>; it is
  the first code that runs when someone starts the AppImage.
- **cargo-deny:** version and SHA-256 in `.github/workflows/ci.yml`; its
  rules are in `deny.toml`.
