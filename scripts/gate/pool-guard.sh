#!/usr/bin/env bash
# The package versions against the latest release, before a push: CI's
# "package versions" step (mica-tools pool guard) over a fresh amd64 pool of
# this checkout. A package at its released version must keep its inputs and
# bytes, so a change to a producer after a release needs its version bumped.
# amd64 only: the local arm64 build is a cross build, whose bytes are not CI's.
set -euo pipefail
cd "$(dirname "$0")/../.."
mkdir -p _out
pools="$(mktemp -d _out/pool-guard.XXXXXX)"
trap 'rm -rf "${pools}"' EXIT
rows="$(bash scripts/deb/producers.sh)"
for producer in $(printf '%s\n' "${rows}" | cut -d' ' -f1); do
    MICA_POOL_DIR="${PWD}/${pools}" bash scripts/deb/build.sh --producer "${producer}" --arch amd64
done
for file in "${pools}/amd64/pool/"*; do
    bin/mica-tools pool guard amd64 "${file}"
done
echo "pool guard: every amd64 package keeps its released version's inputs and bytes, or has a new version"
