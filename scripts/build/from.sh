#!/usr/bin/env bash
# Image references out of locks/mica-build-env.lock, by digest: the only place
# this repository takes an image from.
#
#   bash scripts/build/from.sh --ref <image>                 a mica-build-env image (base, rust), its index
#   bash scripts/build/from.sh --arch=<amd64|arm64> --ref <image>   that image's platform manifest
#   bash scripts/build/from.sh --upstream <name>             a third-party image by its original name
#                                                            (moby/buildkit:v0.33.0), from the lock's upstream rows
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
LOCK="${MICA_LOCKS_DIR:-${REPO_ROOT}/locks}/mica-build-env.lock"
die() { echo "from.sh: error: $*" >&2; exit 1; }
[ -f "${LOCK}" ] || die "${LOCK} does not exist"

row() { # <source> <name> <platform>
    awk -F'\t' -v s="$1" -v n="$2" -v p="$3" '$1 == "image" && $2 == s && $3 == n && $4 == p { print $5; found = 1 } END { exit !found }' "${LOCK}"
}

platform=index
case "${1-}" in --arch=amd64 | --arch=arm64) platform="${1#--arch=}"; shift ;; --arch=*) die "$1 is not amd64 or arm64" ;; esac
case "${1-}" in
--ref)
    [ "$#" -eq 2 ] || die "usage: from.sh [--arch=<amd64|arm64>] --ref <image>"
    row mica-build-env "$2" "${platform}" || die "${LOCK} has no image row for mica-build-env $2 ${platform}"
    ;;
--upstream)
    [ "$#" -eq 2 ] && [ "${platform}" = index ] || die "usage: from.sh --upstream <name>"
    # An upstream row names the index digest on every platform row; the amd64 row is always present.
    row upstream "$2" amd64 || die "${LOCK} lists no upstream image $2; a third-party image is taken only from mica-build-env's upstream rows"
    ;;
*) die "usage: from.sh [--arch=<amd64|arm64>] --ref <image> | --upstream <name>" ;;
esac
