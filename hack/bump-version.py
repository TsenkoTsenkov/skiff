#!/usr/bin/env python3
"""Makes the next stable release: bumps the version and dates the changelog.

    hack/bump-version.py            # 0.1.2 -> 0.1.3
    hack/bump-version.py minor      # 0.1.2 -> 0.2.0
    hack/bump-version.py major      # 0.1.2 -> 1.0.0
    hack/bump-version.py 0.4.0      # exactly that

It changes five files and nothing else:

- Cargo.toml: `[workspace.package] version`;
- Cargo.lock: the version of every crate in crates/;
- Casks/skiff.rb (Homebrew) and bucket/skiff.json (Scoop): the version
  they install, so that `brew upgrade` and `scoop update` find it;
- CHANGELOG.md: the "Unreleased" entries become the new version's
  section, dated today, under a new empty "Unreleased", and the compare
  links at the bottom follow.

Commit that with the change it releases. When it reaches main, the
release workflow sees a version without a tag and publishes it as the
stable release vX.Y.Z (see .github/workflows/release.yml). A change
merged without a bump is published as a nightly only.
"""

import datetime
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent


def die(msg: str) -> None:
    sys.exit(f"bump-version: {msg}")


def main() -> None:
    what = sys.argv[1] if len(sys.argv) > 1 else "patch"
    cargo = ROOT / "Cargo.toml"
    text = cargo.read_text()
    m = re.search(r'^\[workspace\.package\]\n(?:.*\n)*?version = "(\d+)\.(\d+)\.(\d+)"', text, re.M)
    if not m:
        die("no [workspace.package] version in Cargo.toml")
    major, minor, patch = (int(g) for g in m.groups())
    old = f"{major}.{minor}.{patch}"
    if what == "patch":
        new = f"{major}.{minor}.{patch + 1}"
    elif what == "minor":
        new = f"{major}.{minor + 1}.0"
    elif what == "major":
        new = f"{major + 1}.0.0"
    elif re.fullmatch(r"\d+\.\d+\.\d+", what):
        new = what
        if tuple(map(int, new.split("."))) <= (major, minor, patch):
            die(f"{new} is not newer than {old}")
    else:
        die(f"expected patch, minor, major or X.Y.Z, not {what!r}")

    # Cargo.toml
    start, end = m.span(0)
    cargo.write_text(text[:start] + m.group(0).replace(f'"{old}"', f'"{new}"') + text[end:])

    # Cargo.lock: the workspace's own crates.
    lock = ROOT / "Cargo.lock"
    ltext = lock.read_text()
    for manifest in sorted((ROOT / "crates").glob("*/Cargo.toml")):
        name = re.search(r'^name = "([^"]+)"', manifest.read_text(), re.M).group(1)
        pattern = f'name = "{name}"\nversion = "{old}"\n'
        if ltext.count(pattern) != 1:
            die(f"Cargo.lock: no single entry {name} {old}")
        ltext = ltext.replace(pattern, f'name = "{name}"\nversion = "{new}"\n')
    lock.write_text(ltext)

    # The Homebrew cask and the Scoop manifest (this repository is their tap
    # and bucket). They carry no checksum, so the version is all that changes.
    for rel, pattern, repl in (
        ("Casks/skiff.rb", f'  version "{old}"\n', f'  version "{new}"\n'),
        ("bucket/skiff.json", f'"version": "{old}"', f'"version": "{new}"'),
        ("bucket/skiff.json", f"/download/v{old}/", f"/download/v{new}/"),
    ):
        path = ROOT / rel
        ptext = path.read_text()
        if ptext.count(pattern) != 1:
            die(f"{rel}: no single {pattern.strip()!r}")
        path.write_text(ptext.replace(pattern, repl))

    # CHANGELOG.md
    log = ROOT / "CHANGELOG.md"
    ctext = log.read_text()
    if f"## [{new}]" in ctext:
        die(f"CHANGELOG.md already has a section for {new}")
    head = "## [Unreleased]\n"
    if ctext.count(head) != 1:
        die("CHANGELOG.md: no single '## [Unreleased]' heading")
    after = ctext.split(head, 1)[1]
    entries = after.split("\n## ", 1)[0].strip()
    if not entries:
        print("bump-version: note: 'Unreleased' is empty; the release will have no notes", file=sys.stderr)
    today = datetime.date.today().isoformat()
    ctext = ctext.replace(head, f"{head}\n## [{new}] - {today}\n", 1)
    link = re.search(r"^\[Unreleased\]: (\S+)/compare/v[^.\s]+\.[^.\s]+\.[^.\s]+\.\.\.HEAD$", ctext, re.M)
    if link:
        base = link.group(1)
        ctext = ctext.replace(
            link.group(0),
            f"[Unreleased]: {base}/compare/v{new}...HEAD\n[{new}]: {base}/compare/v{old}...v{new}",
        )
    log.write_text(ctext)
    print(f"{old} -> {new}")


if __name__ == "__main__":
    main()
