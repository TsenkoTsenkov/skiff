#!/usr/bin/env python3
"""Turns the file names in a release's "Downloads" table into links.

    link-release-downloads.py <owner/repo> <tag> <asset>... < notes.md > new.md

Releases published before the release workflow wrote those links name
their files as plain text (`Skiff-macos-universal.dmg`). This rewrites
such notes: in the rows of a table, a name in backquotes that is one of
the release's files becomes a link that downloads it. Everything else,
including names that are already links, is left as it is, so running it
twice changes nothing.
"""
import re
import sys

NAME = re.compile(r"(?<!\[)`([^`\s]+)`")


def link(notes, repo, tag, assets):
    assets = set(assets)

    def replace(match):
        name = match.group(1)
        if name not in assets:
            return match.group(0)
        return f"[`{name}`](https://github.com/{repo}/releases/download/{tag}/{name})"

    lines = notes.split("\n")
    return "\n".join(NAME.sub(replace, line) if line.lstrip().startswith("|") else line for line in lines)


if __name__ == "__main__":
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    sys.stdout.write(link(sys.stdin.read(), sys.argv[1], sys.argv[2], sys.argv[3:]))
