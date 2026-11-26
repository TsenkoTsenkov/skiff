#!/usr/bin/env python3
"""Writes the winget manifests and the AUR package of a stable release.

    hack/package-manifests.py 0.1.4 --sums SHA256SUMS --date 2026-11-25 --out DIR

Reads the hashes from the release's SHA256SUMS (download it first, or let
.github/workflows/package-managers.yml do it) and writes:

    DIR/winget/TsenkoTsenkov.Skiff*.yaml   packaging/winget/*.yaml, filled in
    DIR/aur/PKGBUILD, DIR/aur/.SRCINFO     packaging/aur/PKGBUILD.in, filled in

The Homebrew cask (Casks/skiff.rb) and the Scoop manifest (bucket/skiff.json)
need no hash and are kept current by hack/bump-version.py instead.
Standard library only.
"""

import argparse
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent


def die(msg):
    sys.exit(f"package-manifests: {msg}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("version")
    ap.add_argument("--sums", required=True, help="the release's SHA256SUMS")
    ap.add_argument("--date", required=True, help="release date, YYYY-MM-DD")
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    if not re.fullmatch(r"\d+\.\d+\.\d+", a.version):
        die(f"not a version: {a.version}")
    if not re.fullmatch(r"\d{4}-\d{2}-\d{2}", a.date):
        die(f"not a date: {a.date}")

    sums = {}
    for line in pathlib.Path(a.sums).read_text().splitlines():
        m = re.fullmatch(r"([0-9a-f]{64})\s+\*?(\S+)", line.strip())
        if m:
            sums[m.group(2)] = m.group(1)

    def sha(name):
        if name not in sums:
            die(f"{name} is not in {a.sums}")
        return sums[name]

    out = pathlib.Path(a.out)

    # winget: replace the template's version, date and hash; drop the
    # template comments that explain how to fill it in.
    wdir = out / "winget"
    wdir.mkdir(parents=True, exist_ok=True)
    for src in sorted((ROOT / "packaging/winget").glob("*.yaml")):
        text = src.read_text()
        text = "\n".join(l for l in text.splitlines() if not l.startswith("# ") or "yaml-language-server" in l) + "\n"
        text = re.sub(r"^PackageVersion: .*$", f"PackageVersion: {a.version}", text, flags=re.M)
        text = re.sub(r"^ReleaseDate: .*$", f"ReleaseDate: {a.date}", text, flags=re.M)
        text = re.sub(r"/v\d+\.\d+\.\d+/", f"/v{a.version}/", text)
        text = re.sub(r"/tag/v\d+\.\d+\.\d+$", f"/tag/v{a.version}", text, flags=re.M)
        text = re.sub(r"^(\s*InstallerSha256: ).*$", lambda m: m.group(1) + sha("Skiff-windows-x64-setup.exe").upper(), text, flags=re.M)
        if "0.1.0" in text or "000000000000" in text:
            die(f"{src.name}: a template value was left in")
        (wdir / src.name).write_text(text)

    # AUR
    adir = out / "aur"
    adir.mkdir(parents=True, exist_ok=True)
    pkgbuild = (ROOT / "packaging/aur/PKGBUILD.in").read_text()
    pkgbuild = "\n".join(l for l in pkgbuild.splitlines() if not (l.startswith("#") and not l.startswith("# Maintainer"))) + "\n"
    x86, arm = sha("skiff-linux-x86_64.tar.gz"), sha("skiff-linux-aarch64.tar.gz")
    pkgbuild = pkgbuild.replace("@VERSION@", a.version).replace("@SHA256_X86_64@", x86).replace("@SHA256_AARCH64@", arm)
    if "@" in re.sub(r"#.*", "", pkgbuild):
        die("PKGBUILD: a placeholder was left in")
    (adir / "PKGBUILD").write_text(pkgbuild)

    # .SRCINFO, as `makepkg --printsrcinfo` would write it for this PKGBUILD.
    def field(name):
        m = re.search(rf"^{name}=(.*)$", pkgbuild, re.M)
        return m.group(1).strip("'\"")

    base = "https://github.com/TsenkoTsenkov/skiff/releases/download"
    srcinfo = [
        "pkgbase = skiff-bin",
        f"\tpkgdesc = {field('pkgdesc')}",
        f"\tpkgver = {a.version}",
        "\tpkgrel = 1",
        f"\turl = {field('url')}",
        "\tarch = x86_64",
        "\tarch = aarch64",
        "\tlicense = MIT",
    ]
    for dep in ("gcc-libs", "glibc", "libxkbcommon", "fontconfig"):
        srcinfo.append(f"\tdepends = {dep}")
    for opt in re.findall(r"'([^']+: [^']+)'", pkgbuild.split("optdepends=(", 1)[1].split(")", 1)[0]):
        srcinfo.append(f"\toptdepends = {opt}")
    srcinfo += [
        "\tprovides = skiff",
        "\tconflicts = skiff",
        "\toptions = !strip",
        "\toptions = !debug",
        f"\tsource_x86_64 = skiff-{a.version}-x86_64.tar.gz::{base}/v{a.version}/skiff-linux-x86_64.tar.gz",
        f"\tsha256sums_x86_64 = {x86}",
        f"\tsource_aarch64 = skiff-{a.version}-aarch64.tar.gz::{base}/v{a.version}/skiff-linux-aarch64.tar.gz",
        f"\tsha256sums_aarch64 = {arm}",
        "",
        "pkgname = skiff-bin",
        "",
    ]
    (adir / ".SRCINFO").write_text("\n".join(srcinfo))
    print(f"wrote {wdir} and {adir}")


if __name__ == "__main__":
    main()
