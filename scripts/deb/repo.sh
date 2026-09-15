#!/usr/bin/env bash
# Index one architecture's pool: Packages, SHA256SUMS and manifest.txt beside
# _out/debs/<arch>/pool, read out of the archives in the mica-build-env base image.
#
#   bash scripts/deb/repo.sh --arch <amd64|arm64>
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
die() { echo "repo.sh: error: $*" >&2; exit 1; }
command -v docker >/dev/null 2>&1 || die "docker is required"
[ "${1:-}" = --arch ] || die "usage: bash scripts/deb/repo.sh --arch <amd64|arm64>"
ARCH="${2:-}"
case "${ARCH}" in amd64 | arm64) ;; *) die "--arch must be amd64 or arm64" ;; esac

DIST="${MICA_POOL_DIR:-${REPO_ROOT}/_out/debs}/${ARCH}"
[ -n "$(find "${DIST}/pool" -maxdepth 1 -name '*.deb' 2>/dev/null)" ] || die "${DIST}/pool holds no archive"
case "$(uname -m)" in x86_64) HOST_ARCH=amd64 ;; aarch64 | arm64) HOST_ARCH=arm64 ;; *) die "unsupported host $(uname -m)" ;; esac
IMAGE="$(bash "${REPO_ROOT}/scripts/build/from.sh" --arch="${HOST_ARCH}" --ref base)"

docker run --rm --label ai-agent=true -v "${DIST}:/dist" -w /dist -e "ARCH=${ARCH}" \
    --entrypoint /bin/bash "${IMAGE}" -c '
        set -euo pipefail
        mapfile -t debs < <(find pool -maxdepth 1 -type f -name "*.deb" -printf "%f\n" | LC_ALL=C sort)
        for d in "${debs[@]}"; do
            a="$(dpkg-deb --field "pool/${d}" Architecture)"
            [ "${a}" = "${ARCH}" ] || { echo "repo.sh: error: pool/${d} is Architecture ${a}, not ${ARCH}" >&2; exit 1; }
        done
        dpkg-scanpackages --multiversion pool >Packages
        [ -s Packages ] || { echo "repo.sh: error: dpkg-scanpackages wrote nothing" >&2; exit 1; }
        printf "pool/%s\n" "${debs[@]}" | xargs sha256sum >SHA256SUMS
        {
            printf "#package\tversion\tarchitecture\tinstalled-size\tsha256\tfile\tsource-repo\tsource-commit\n"
            for d in "${debs[@]}"; do
                f() { dpkg-deb --field "pool/${d}" "$1"; }
                [ -n "$(f Mica-Source-Commit)" ] || { echo "repo.sh: error: pool/${d} carries no Mica-Source-Commit" >&2; exit 1; }
                printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" "$(f Package)" "$(f Version)" "$(f Architecture)" "$(f Installed-Size)" \
                    "$(sha256sum "pool/${d}" | cut -d" " -f1)" "pool/${d}" "$(f Mica-Source-Repo)" "$(f Mica-Source-Commit)"
            done
        } >manifest.txt
        echo "repo.sh: ${ARCH}: ${#debs[@]} package(s)"
    '
sed 's/^/  /' "${DIST}/manifest.txt"
