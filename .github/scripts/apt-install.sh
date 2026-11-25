#!/usr/bin/env bash
# Installs Ubuntu packages on a CI runner:
#
#     .github/scripts/apt-install.sh <package>...
#
# `apt-get update` answers "fine" even when it could not fetch a package
# list, and the install that follows then fails with "Unable to locate
# package", a red run that has nothing to do with the change under test.
# So: a list that cannot be fetched is an error here, and the whole thing
# is tried up to four times, waiting a little longer each time.
set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: $0 <package>..." >&2
  exit 2
fi

for attempt in 1 2 3 4; do
  if sudo apt-get -o Acquire::Retries=3 update --error-on=any &&
    sudo apt-get -o Acquire::Retries=3 install -y --no-install-recommends "$@"; then
    exit 0
  fi
  if [ "$attempt" -lt 4 ]; then
    wait=$((attempt * 15))
    echo "::warning::Installing packages failed (attempt $attempt of 4); trying again in $wait s."
    sleep "$wait"
  fi
done
echo "::error::Could not install: $*"
exit 1
