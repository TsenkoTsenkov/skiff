# Installing Skiff

Every release carries the same file names, so these links always point at
the current stable release:
`https://github.com/TsenkoTsenkov/skiff/releases/latest/download/<file>`.

| System | File |
| --- | --- |
| macOS 11 or newer (Apple Silicon and Intel, one universal app) | `Skiff-macos-universal.dmg` |
| Windows 10 or 11 (x64), installer | `Skiff-windows-x64-setup.exe` |
| Windows 10 or 11 (x64), portable | `Skiff-windows-x64.zip` |
| Linux, any distribution | `Skiff-linux-x86_64.AppImage`, `Skiff-linux-aarch64.AppImage` |
| Debian, Ubuntu | `skiff_amd64.deb`, `skiff_arm64.deb` |
| Linux, plain archive | `skiff-linux-x86_64.tar.gz`, `skiff-linux-aarch64.tar.gz` |
| Checksums of all of the above | `SHA256SUMS` |

The two `Skiff-macos-…-build-<N>.zip` files on a release are for the
in-app updater; you do not need them.

## macOS

1. Open `Skiff-macos-universal.dmg` and drag Skiff to Applications.
2. Open Skiff from Applications.

**While builds are not signed with an Apple Developer ID**, macOS refuses
the first start ("Skiff cannot be opened…"). Allow it once: System Settings
→ Privacy & Security → scroll down to the message about Skiff → **Open
Anyway**. From a terminal the same is done with:

```bash
xattr -dr com.apple.quarantine /Applications/Skiff.app
```

The notes of each release say whether that build is signed and notarized;
a notarized build opens like any other downloaded app.

Opened from the Finder or the Dock, Skiff asks your login shell once for
`PATH`, `KUBECONFIG` and the proxy variables, so that the credential
plugins your kubeconfig calls (`aws`, `gke-gcloud-auth-plugin`,
`kubelogin`, …) are found. Nothing else is taken from the shell.

## Windows

Run `Skiff-windows-x64-setup.exe`. It installs for the current user into
`%LOCALAPPDATA%\Programs\Skiff`, adds a Start menu entry and an uninstaller
(Settings → Apps), and does not ask for administrator rights.

**While builds are not signed**, Windows shows "Windows protected your PC"
(SmartScreen). Choose **More info**, then **Run anyway**.

The portable `Skiff-windows-x64.zip` holds `skiff.exe` and the licence; unpack it
anywhere and run it.

## Linux

Skiff needs glibc 2.35 or newer (Ubuntu 22.04, Debian 12 and later), a
Vulkan or OpenGL driver, and `libxkbcommon`. No GPU, Wayland or X11
libraries are bundled in any of the packages; the system's own are used.
On a machine without a GPU driver (a VM), install a software one:
`mesa-vulkan-drivers` on Debian and Ubuntu.

**AppImage**

```bash
chmod +x Skiff-linux-x86_64.AppImage
./Skiff-linux-x86_64.AppImage
```

**Debian and Ubuntu**

```bash
sudo apt install ./skiff_amd64.deb
```

This installs `/usr/bin/skiff`, a desktop entry and icons, and pulls in
`libxkbcommon0`; the graphics and windowing libraries are recommended
packages that a desktop already has.

**Archive**

```bash
tar -xzf skiff-linux-x86_64.tar.gz --strip-components=1 -C ~/.local
```

puts `skiff` into `~/.local/bin` with its desktop entry and icons next to
it.

The file dialog of "Add kubeconfig file…" uses the desktop portal (D-Bus)
or `zenity`; without either, Skiff asks for the path in a dialog of its
own.

If Skiff cannot start its default renderer it tries OpenGL; if both fail
it says so and exits. `SKIFF_RENDERER=glow` (or `wgpu`) forces one, see
[configuration.md](configuration.md#environment-variables).

## Updating

Skiff looks for a new version when it starts and every four hours while it
is open. The request goes to GitHub's API, anonymously.

- **macOS:** an **Update** button appears in the top bar. It downloads the
  new version next to the installed one, checks its code signature, and
  **Restart to update** swaps the two and reopens Skiff. A copy that was
  never moved out of the download (macOS then runs it from a read-only
  place) says "Move Skiff to Applications to enable updates".
  What the swap did is logged to `~/Library/Logs/Skiff-update.log`.
- **Windows and Linux:** a **New version** button appears. It opens the
  release page, where you download and install the new version as above.
  Skiff does not replace itself on these systems.

"Check for updates" is also in the command palette (`Ctrl/Cmd+K`), and
Settings → Updates shows the version, build number and channel.

**Channels.** *Stable* (the default) offers released versions, tagged
`vX.Y.Z`. *Nightly* offers every build of the `main` branch, which may be
rough. Choose in Settings → Updates, or in `config.json`:

```json
{ "updates": { "check": true, "channel": "nightly" } }
```

**Turning the check off.** Settings → Updates, `"updates": {"check":
false}` in `config.json`, or the environment variable
`SKIFF_NO_UPDATE_CHECK` set to anything. "Check for updates" in the
palette still works when asked.

## Checking a download

Download `SHA256SUMS` from the same release into the folder with the
file, then:

```bash
sha256sum -c SHA256SUMS --ignore-missing        # Linux
shasum -a 256 -c SHA256SUMS --ignore-missing    # macOS
```

```powershell
(Get-FileHash .\Skiff-windows-x64-setup.exe -Algorithm SHA256).Hash   # compare with the line in SHA256SUMS
```

A checksum shows the file arrived intact. That it was built by this
repository's release workflow, from the tagged source, is shown by its
build provenance attestation, checked with the
[GitHub CLI](https://cli.github.com):

```bash
gh attestation verify Skiff-linux-x86_64.AppImage -R TsenkoTsenkov/skiff
```

GitHub provides attestations for public repositories only. A release made
while the repository was private has none, and its notes say so.

## Building from source

You need Rust 1.95 or newer (`rust-toolchain.toml` pins the version
releases are built with; `rustup` installs it on first use).

```bash
git clone https://github.com/TsenkoTsenkov/skiff
cd skiff
cargo build --release
./target/release/skiff
```

- **Linux** additionally needs `pkg-config` and `libx11-dev` to build, and
  at run time what the packages need (above).
- **macOS** needs the Xcode command line tools (`xcode-select --install`).
  `hack/macos-app.sh --install` builds `Skiff.app` and puts it into
  `/Applications`; `--dmg` also makes the disk image.
- **Windows** needs the MSVC build tools. `hack/windows-package.ps1`
  builds the installer and the zip.
- `hack/linux-package.sh` builds the Linux packages into `dist/`.

A build made this way has build number 0 and is not signed. Its update
check compares version numbers only.

The packaging scripts and the release pipeline are described in
[packaging/README.md](../packaging/README.md).
