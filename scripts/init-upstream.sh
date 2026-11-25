#!/usr/bin/env bash
# Initialize and pin the upstream repository as the `upstream/` submodule.
#
# Usage: bash scripts/init-upstream.sh <upstream-url> [commit]
#
# Run once at the start of a new port, before any inventory work. The script
# does not commit anything: commit the submodule entry as its own ledger unit
# and record the pin in PLAN.md.
set -euo pipefail

url="${1:-}"
commit="${2:-}"

if [[ -z $url   ]]; then
  echo "usage: bash $0 <upstream-url> [commit]" >&2
  exit 2
fi

submodule_registered() {
  git config -f .gitmodules --get-regexp '^submodule\..*\.path$' 2> /dev/null \
    | awk '{print $2}' \
    | grep -qx 'upstream'
}

if submodule_registered; then
  git submodule update --init upstream
elif [[ -e upstream ]]; then
  echo "upstream/ exists but is not a registered submodule; remove or relocate it first" >&2
  exit 1
else
  git submodule add "$url" upstream
fi

if [[ -n $commit   ]]; then
  git -C upstream checkout "$commit"
fi

pin="$(git -C upstream rev-parse HEAD)"
describe="$(git -C upstream describe --tags --always 2> /dev/null || echo 'no tags')"
echo "upstream pinned at ${pin} (${describe})"
echo "Next: record the pin under 'Progress snapshot' in PLAN.md and run S0 Recon."
