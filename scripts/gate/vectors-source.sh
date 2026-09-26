#!/usr/bin/env bash
# The release-lock vectors at the commit scripts/gate/vectors.pin names, in the
# git-ignored source cache. Prints the directory; fetches only when the cache
# does not already hold that commit, so a warm cache and `make offline` need no
# network.
#
# The vectors are READ, never copied into this tree: a copy is a snapshot that
# drifts from the specification.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PIN="${REPO_ROOT}/scripts/gate/vectors.pin"
CACHE="${MICA_VECTORS_CACHE:-${REPO_ROOT}/repos}"
die() { echo "vectors-source.sh: error: $*" >&2; exit 1; }

[ -f "${PIN}" ] || die "${PIN#"${REPO_ROOT}"/} does not exist; it names the mica commit the vectors are read at"
REPOSITORY="$(sed -n 's/^REPOSITORY=//p' "${PIN}")"
COMMIT="$(sed -n 's/^COMMIT=//p' "${PIN}")"
[ "${REPOSITORY}" = mica ] || die "${PIN#"${REPO_ROOT}"/} names REPOSITORY=${REPOSITORY:-}, and the vectors live in mica"
[[ "${COMMIT}" =~ ^[0-9a-f]{40}$ ]] || die "${PIN#"${REPO_ROOT}"/} names no full 40-hex COMMIT (a short commit is not a pin: git resolves it against whatever it has)"

WORKTREE="${CACHE}/${REPOSITORY}/${COMMIT}"
VECTORS="${WORKTREE}/docs/design/release-lock/vectors"
if [ ! -d "${VECTORS}" ]; then
    # A local checkout of the same repository is a valid source of the commit:
    # it is verified by hash below, so where the object came from cannot matter.
    SOURCE="${MICA_VECTORS_GIT:-https://github.com/micaoss/${REPOSITORY}.git}"
    mkdir -p "${WORKTREE}"
    git -C "${WORKTREE}" init -q 2>/dev/null || die "cannot initialise ${WORKTREE}"
    git -C "${WORKTREE}" fetch -q --depth 1 "${SOURCE}" "${COMMIT}" ||
        die "cannot fetch ${REPOSITORY} ${COMMIT} from ${SOURCE}; with no network, populate ${CACHE#"${REPO_ROOT}"/}/${REPOSITORY}/${COMMIT} from a checkout"
    git -C "${WORKTREE}" checkout -q FETCH_HEAD
    [ -d "${VECTORS}" ] || die "${REPOSITORY} ${COMMIT} carries no docs/design/release-lock/vectors"
fi
# git refuses an object that does not hash to its name, so the checkout is the
# pin: what is verified is the commit, not the directory it was written into.
HEAD="$(git -C "${WORKTREE}" rev-parse HEAD)"
[ "${HEAD}" = "${COMMIT}" ] || die "${WORKTREE} is at ${HEAD}, not the pinned ${COMMIT}"
printf '%s\n' "${VECTORS}"
