#!/usr/bin/env bash
# The ${shlibs:Depends} of a staged package tree, for `mica-tools deb pack --substitute`:
# dpkg-shlibdeps over every ELF file under <root>, in the build image of the
# archive's architecture, whose libraries it resolves.
#
#   bash scripts/deb/shlibs.sh <root>
#
# Prints the value on stdout. A tree with no ELF file, or one whose ELF files
# resolve no shared library, is an error: a template asking for the
# substitution would be left with a dangling separator.
set -euo pipefail

die() { echo "shlibs.sh: error: $*" >&2; exit 1; }
ROOT="${1:?usage: bash scripts/deb/shlibs.sh <root>}"
[ -d "${ROOT}" ] || die "${ROOT} is not a directory"
ROOT="$(cd "${ROOT}" && pwd)"

ELVES=()
while IFS= read -r -d '' f; do
    case "$(file -b "${f}")" in ELF*) ELVES+=("${f}") ;; esac
done < <(find "${ROOT}" -path "${ROOT}/DEBIAN" -prune -o -type f -print0)
[ "${#ELVES[@]}" -gt 0 ] || die "no ELF executable or shared object under ${ROOT}"

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
ARCH="$(dpkg --print-architecture)"
mkdir -p "${WORK}/debian"
printf 'Source: shlibs\n\nPackage: shlibs\nArchitecture: %s\n' "${ARCH}" >"${WORK}/debian/control"
out="$(cd "${WORK}" && DEB_HOST_ARCH="${ARCH}" DEB_BUILD_ARCH="${ARCH}" dpkg-shlibdeps -O "${ELVES[@]}")"
value="$(printf '%s\n' "${out}" | sed -n 's/^shlibs:Depends=//p')"
[ -n "${value}" ] || die "dpkg-shlibdeps resolved no shared library across ${#ELVES[@]} ELF file(s) under ${ROOT}"
printf '%s\n' "${value}"
