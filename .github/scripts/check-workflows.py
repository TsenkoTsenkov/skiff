#!/usr/bin/env python3
"""Checks properties of the workflows that a linter does not know about.

Run from the repository root (the CI job "packaging" does):

    python3 .github/scripts/check-workflows.py

1. The Rust toolchain's name is read from rust-toolchain.toml with sed. On
   Windows the file may be checked out with CRLF line endings; the name must
   come out the same, without a carriage return at its end.
2. In release.yml nothing is compiled within reach of a signing secret: once
   a step of a job has been given a secret, no step of that job runs cargo
   or a packaging script in the mode that builds.
3. In release.yml only the publish job may write, it never runs for a build
   that is not to be published, and it is never cancelled half-way.

Needs PyYAML (python3-yaml).
"""
import glob
import re
import subprocess
import sys
import tempfile

import yaml

errors = []


def error(message):
    errors.append(message)


def load(path):
    with open(path, encoding="utf-8") as f:
        return yaml.safe_load(f)


# ---- 1. the toolchain's name survives CRLF --------------------------------

SED = re.compile(r"sed -n '([^']*)' rust-toolchain\.toml")


def check_toolchain_sed():
    with open("rust-toolchain.toml", "rb") as f:
        lf = f.read().replace(b"\r\n", b"\n")
    want = re.search(rb'^channel = "([^"]+)"', lf, re.M).group(1) + b"\n"
    found = 0
    for path in sorted(glob.glob(".github/workflows/*.yml")):
        with open(path, encoding="utf-8") as f:
            text = f.read()
        for expression in sorted(set(SED.findall(text))):
            found += 1
            for name, content in (("LF", lf), ("CRLF", lf.replace(b"\n", b"\r\n"))):
                with tempfile.NamedTemporaryFile() as tmp:
                    tmp.write(content)
                    tmp.flush()
                    got = subprocess.run(
                        ["sed", "-n", expression, tmp.name], check=True, capture_output=True
                    ).stdout
                if got != want:
                    error(
                        f"{path}: sed -n '{expression}' gives {got!r} for a rust-toolchain.toml "
                        f"with {name} line endings; expected {want!r}"
                    )
    if found == 0:
        error("no workflow reads rust-toolchain.toml with sed any more: update this check")


# ---- 2. nothing is compiled within reach of a secret ----------------------

# "secrets.X != ''" only asks whether a secret exists; it gives nothing away.
EXISTS = re.compile(r"secrets\.[A-Z0-9_]+\s*!=\s*''")
COMPILES = (
    (re.compile(r"\bcargo\s+(build|check|test|run|install|clippy|doc|bench)\b"), "runs cargo"),
    (re.compile(r"macos-app\.sh(?![^\n]*--no-build)"), "runs hack/macos-app.sh without --no-build"),
    (re.compile(r"linux-package\.sh(?![^\n]*--binary)"), "runs hack/linux-package.sh without --binary"),
    (re.compile(r"windows-package\.ps1(?![^\n]*-Exe)"), "runs hack/windows-package.ps1 without -Exe"),
)


def holds_secret(env):
    return any("secrets." in EXISTS.sub("", str(v)) for v in (env or {}).values())


def check_no_compiling_near_secrets(path, workflow):
    if holds_secret(workflow.get("env")):
        error(f"{path}: a secret in the workflow's env reaches every step")
    for job_id, job in workflow["jobs"].items():
        reach = holds_secret(job.get("env"))
        for step in job.get("steps", []):
            name = step.get("name") or step.get("uses") or "?"
            # (A secret passed to an action counts too.)
            reach = reach or holds_secret(step.get("env")) or holds_secret(step.get("with"))
            if not reach:
                continue
            for pattern, what in COMPILES:
                if pattern.search(step.get("run") or ""):
                    error(
                        f"{path}: job '{job_id}', step '{name}' {what} after a secret was given to "
                        "this job; compile in an earlier step that has no secret"
                    )


# ---- 3. who may write, and when -------------------------------------------


def check_release_permissions(path, workflow):
    if workflow.get("permissions") != {"contents": "read"}:
        error(f"{path}: the workflow's permissions must be exactly 'contents: read'")
    if "concurrency" in workflow:
        error(f"{path}: workflow-level concurrency would let a newer run cancel a publish half-way")
    for job_id, job in workflow["jobs"].items():
        if job_id == "publish":
            if "channel != 'none'" not in str(job.get("if", "")):
                error(f"{path}: the publish job must not run when the channel is 'none'")
            if (job.get("concurrency") or {}).get("cancel-in-progress") is not False:
                error(f"{path}: the publish job must have a concurrency group that is never cancelled")
        else:
            if "permissions" in job:
                error(f"{path}: job '{job_id}' asks for permissions; only publish may")
            if job.get("environment") == "release":
                error(f"{path}: job '{job_id}' always uses the 'release' environment, also for builds that are not published")


def main():
    check_toolchain_sed()
    release = ".github/workflows/release.yml"
    workflow = load(release)
    check_no_compiling_near_secrets(release, workflow)
    check_release_permissions(release, workflow)
    for message in errors:
        print(f"error: {message}", file=sys.stderr)
    if errors:
        return 1
    print("ok: workflows")
    return 0


if __name__ == "__main__":
    sys.exit(main())
