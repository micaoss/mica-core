#!/usr/bin/env bash
# The inputs this repository builds from, in locks/ (mica:docs/design/release-lock.md
# section 4): locks/mica-build-env.lock with locks/pins/mica-build-env.pin, and
# locks/upstream.lock.
#
#   bash scripts/build/locks.sh check     the file rules of every lock and pin, offline
#   bash scripts/build/locks.sh verify    check, then every pinned lock is its release's,
#                                         read anonymously from GitHub
#
# Moving to another release of an input replaces locks/<repository>.lock and
# locks/pins/<repository>.pin together.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
LOCKS="${MICA_LOCKS_DIR:-${REPO_ROOT}/locks}"
RELEASES="${MICA_LOCKS_RELEASES:-https://github.com/micaoss}"
CHECK="${REPO_ROOT}/scripts/build/check-lock.sh"
die() { echo "locks.sh: error: $*" >&2; exit 1; }

check() {
    local mode=local result
    [ -z "${CI:-}${GITHUB_ACTIONS:-}" ] || mode=ci
    [ -f "${LOCKS}/mica-build-env.lock" ] || die "${LOCKS}/mica-build-env.lock does not exist; it is the lock asset of the mica-build-env release this repository builds on"
    result="$(bash "${CHECK}" pins "${LOCKS}" "${mode}")" || die "${LOCKS} is ${result}"
    result="$(bash "${CHECK}" upstream "${LOCKS}/upstream.lock")" || die "${LOCKS}/upstream.lock is ${result}"
    echo "locks.sh: ${LOCKS} is well formed"
}

verify() {
    local pin repository release trust work have
    work="$(mktemp -d)"
    trap 'rm -rf "${work}"' RETURN
    for pin in "${LOCKS}"/pins/*.pin; do
        repository="$(sed -n 's/^REPOSITORY=//p' "${pin}")"
        release="$(sed -n 's/^RELEASE=//p' "${pin}")"
        trust="$(sed -n 's/^SHA256SUMS=//p' "${pin}")"
        [ "${release}" != offline ] || die "${pin} is an offline pin; verify reads published releases only"
        curl -fsSL --retry 3 -o "${work}/SHA256SUMS" "${RELEASES}/${repository}/releases/download/${release}/SHA256SUMS" ||
            die "downloading SHA256SUMS of ${repository} ${release} failed"
        have="$(sha256sum "${work}/SHA256SUMS" | cut -d' ' -f1)"
        [ "${have}" = "${trust}" ] || die "SHA256SUMS of ${repository} ${release} hashes to ${have}, and ${pin} records ${trust}"
        [ "$(sed 's/^[0-9a-f]\{64\}  //' "${work}/SHA256SUMS")" = "${repository}.lock" ] ||
            die "SHA256SUMS of ${repository} ${release} lists $(sed 's/^[0-9a-f]*  //' "${work}/SHA256SUMS" | tr '\n' ' ')rather than exactly ${repository}.lock"
        have="$(sha256sum "${LOCKS}/${repository}.lock" | cut -d' ' -f1)"
        [ "$(cut -d' ' -f1 "${work}/SHA256SUMS")" = "${have}" ] ||
            die "${LOCKS}/${repository}.lock hashes to ${have}, which SHA256SUMS of ${repository} ${release} does not name; it must be that release's asset, unchanged"
        echo "locks.sh: ${repository}.lock is the lock of ${repository} ${release} (SHA256SUMS ${trust:0:12}, lock ${have:0:12}), verified"
    done
}

case "${1-}" in
check) [ "$#" -eq 1 ] || die "usage: bash scripts/build/locks.sh check | verify"; check ;;
verify) [ "$#" -eq 1 ] || die "usage: bash scripts/build/locks.sh check | verify"; check; verify ;;
*) die "usage: bash scripts/build/locks.sh check | verify" ;;
esac
