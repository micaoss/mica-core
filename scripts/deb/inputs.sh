#!/usr/bin/env bash
# The inputs hash of one producer for one architecture: the sha256 of a sorted
# "<sha256>  <path>" manifest of every tracked file in this repository that
# determines the producer's bytes, followed by "arch <arch>"
# (mica:docs/decisions/2026-09-15-package-versions.md R4). It guards against
# inputs that changed without a version bump; it never decides reuse on its own.
#
#   bash scripts/deb/inputs.sh --producer <name> --arch <amd64|arm64> [--manifest]
#
# The files: pkgs/<producer>/ (its producer.env carries VERSION and
# SOURCE_DATE_EPOCH), pkgs/copyright, its CONTEXTS, the workspace manifest,
# Cargo.lock and .cargo/config.toml, every workspace crate in the normal and
# build dependency closure of the crates its binaries come from (their test
# trees and the UI's test files aside), and the tooling that shapes the bytes.
# The build-env images are not inputs: a toolchain that changes bytes is caught
# by the byte-identical rebuild.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
die() { echo "inputs.sh: error: $*" >&2; exit 1; }
for t in git jq sha256sum docker; do command -v "${t}" >/dev/null 2>&1 || die "${t} is required"; done

PRODUCER="" ARCH="" MANIFEST=0
while [ "$#" -gt 0 ]; do
    case "$1" in
    --producer) PRODUCER="${2:?--producer takes a name}"; shift 2 ;;
    --arch) ARCH="${2:?--arch takes amd64 or arm64}"; shift 2 ;;
    --manifest) MANIFEST=1; shift ;;
    *) die "usage: bash scripts/deb/inputs.sh --producer <name> --arch <amd64|arm64> [--manifest]" ;;
    esac
done
case "${ARCH}" in amd64 | arm64) ;; *) die "--arch must be amd64 or arm64" ;; esac
cd "${REPO_ROOT}"
DIR="$(bash scripts/deb/producers.sh --dir-for "${PRODUCER}")"
CONTEXTS=""
# shellcheck disable=SC1091
. "${DIR}/producer.env"
BINS="$(sed -n 's/.*--bins "\([^"]*\)".*/\1/p' "${DIR}/prepare.sh")"
[ -n "${BINS}" ] || die "${DIR}/prepare.sh names no --bins"

# The workspace's own crates, from cargo in the rust image (the host has no cargo).
METADATA="${MICA_INPUTS_METADATA:-}"
if [ -z "${METADATA}" ]; then
    IMAGE="$(bash scripts/build/from.sh --ref rust)"
    METADATA="$(docker run --rm --label ai-agent=true -v "${REPO_ROOT}:/src:ro" -w /src --entrypoint cargo "${IMAGE}" \
        metadata --offline --no-deps --format-version 1)" || die "cargo metadata failed"
fi
mapfile -t CRATES < <(jq -r --arg bins "${BINS}" '
    ($bins | split(" ")) as $wanted
    | .packages as $packages
    | [ $packages[] | select(any(.targets[]; (.kind | index("bin")) and (.name as $n | $wanted | index($n)))) | .name ] as $roots
    | def close($set):
        ([ $packages[] | select(.name as $n | $set | index($n)) | .dependencies[]
           | select(.path != null and (.kind == null or .kind == "build")) | .name ] + $set | unique) as $next
        | if ($next | length) == ($set | length) then $set else close($next) end;
      close($roots | unique)
    | .[] as $name | $packages[] | select(.name == $name) | .manifest_path | sub("/src/"; "") | sub("/Cargo.toml$"; "")' <<<"${METADATA}")
[ "${#CRATES[@]}" -gt 0 ] || die "no crate builds ${BINS}"

PATHS=("${DIR}" pkgs/copyright Cargo.toml Cargo.lock .cargo/config.toml
    scripts/deb/build.sh scripts/deb/pack.sh scripts/build/build-deb.sh "${CRATES[@]}")
for c in ${CONTEXTS}; do PATHS+=("${c#*=}"); done
# Tests, benches and the UI's own test and end-to-end files build nothing a package carries.
EXCLUDE=(':(exclude,glob)crates/*/tests/**' ':(exclude,glob)crates/*/benches/**'
    ':(exclude,glob)crates/mica-apid/ui/e2e/**' ':(exclude,glob)crates/mica-apid/ui/**/*.test.ts'
    ':(exclude,glob)crates/mica-apid/ui/**/*.test.tsx' ':(exclude,glob)crates/mica-apid/ui/src/shared/testing/**'
    ':(exclude,glob)crates/mica-apid/ui/vitest.config.ts' ':(exclude,glob)crates/*/src/tests.rs'
    ':(exclude,glob)crates/*/src/tests/**')

manifest() {
    git ls-files -z -- "${PATHS[@]}" "${EXCLUDE[@]}" | LC_ALL=C sort -zu | while IFS= read -r -d '' path; do
        printf '%s  %s\n' "$(sha256sum -- "${path}" | cut -d' ' -f1)" "${path}"
    done
    printf 'arch %s\n' "${ARCH}"
}
if [ "${MANIFEST}" = 1 ]; then
    manifest
else
    manifest | sha256sum | cut -d' ' -f1
fi
