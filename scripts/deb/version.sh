#!/usr/bin/env bash
# The version of every archive built from this checkout:
# <VERSION>+git<commit12>[.dirty]-1.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
die() { echo "version.sh: error: $*" >&2; exit 1; }

[ "$(grep -c . "${REPO_ROOT}/VERSION")" = 1 ] || die "VERSION must hold exactly one line"
VERSION="$(tr -d '[:space:]' <"${REPO_ROOT}/VERSION")"
[[ "${VERSION}" =~ ^[0-9][A-Za-z0-9.~]*$ ]] || die "VERSION '${VERSION}' is not a Debian upstream version"
COMMIT="$(git -C "${REPO_ROOT}" rev-parse --short=12 HEAD)" || die "not a git checkout"
DIRTY=""
[ -z "$(git -C "${REPO_ROOT}" status --porcelain)" ] || DIRTY=".dirty"
echo "${VERSION}+git${COMMIT}${DIRTY}-1"
