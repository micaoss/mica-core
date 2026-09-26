#!/usr/bin/env bash
# The inputs' readers: scripts/build/check-lock.sh over the specification's
# vectors READ OUT OF mica AT THE COMMIT scripts/gate/vectors.pin NAMES (never
# copied into this tree: a copy is a snapshot that drifts from the
# specification), scripts/build/locks.sh over the committed locks and over
# file:// releases, and scripts/build/from.sh.
#
# WHICH VECTORS MUST PASS IS DERIVED, NOT DECLARED: what this repository pins, what it produces, the vectors that say what
# its own forms may not be, and -- since it now writes one -- the vectors-pin
# family. A vector carrying a row kind this reader does not implement, or a
# scoped release row, is a form nothing here can meet; it is still run, and
# reported with the reason, so the gap is visible on every run rather than
# silently excluded.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CHECK="${REPO_ROOT}/scripts/build/check-lock.sh"
LOCKS="${REPO_ROOT}/scripts/build/locks.sh"
FROM="${REPO_ROOT}/scripts/build/from.sh"
VECTORS="$(bash "${REPO_ROOT}/scripts/gate/vectors-source.sh")"
TAB=$'\t'
PASS=0
FAIL=0
pass() { PASS=$((PASS + 1)); echo "PASS: $1"; }
fail() { FAIL=$((FAIL + 1)); echo "FAIL: $1"; }
mkdir -p "${REPO_ROOT}/tmp"
T="$(mktemp -d "${REPO_ROOT}/tmp/locks-test.XXXXXX")"
trap 'rm -rf "${T}"' EXIT

# The kinds this reader implements. A vector carrying any other kind is outside
# the floor: nothing here reads a lock that may carry one.
KINDS_HERE='^(release|image|pool|package|upstream|data)$'
OUTSIDE=0
ROWS=0
outside_floor() { # <vector path> -> the reason, or empty when it is in the floor
    local file="${VECTORS}/$1" line kind
    case "$1" in pins/* | vectors-pin/*) file="" ;; esac
    case "$1" in pins/*scope*) echo "a scoped pin; this repository pins unscoped releases only"; return ;; esac
    [ -n "${file}" ] && [ -f "${file}" ] || return 0
    local release
    while IFS= read -r line; do
        case "${line}" in '#'* | '') continue ;; esac
        kind="${line%%"${TAB}"*}"
        # A scoped release row (<scope>.<release>) is a form this repository
        # neither pins nor emits, whatever the file is called.
        if [ "${kind}" = release ]; then
            release="$(cut -f3 <<<"${line}")"
            case "${release}" in *.*) echo "a scoped release row (${release}); this repository pins and emits unscoped releases only"; return ;; esac
        fi
        [[ "${kind}" =~ ${KINDS_HERE} ]] || { echo "carries a ${kind} row, a kind this reader does not implement"; return; }
    done <"${file}"
}

# Every row of the canonical manifest is run; what is outside the floor is
# reported rather than skipped, so both directions of the comparison hold.
while IFS="${TAB}" read -r vector want rule mode; do
    case "${vector}" in '#'* | repos/*) continue ;; esac
    ROWS=$((ROWS + 1))
    case "${vector}" in
    pins/*) got="$(bash "${CHECK}" pins "${VECTORS}/${vector}" "${mode}" 2>&1 || true)" ;;
    vectors-pin/*) got="$(bash "${CHECK}" vectors-pin "${VECTORS}/${vector}" 2>&1 || true)" ;;
    *) got="$(bash "${CHECK}" "${vector%%/*}" "${VECTORS}/${vector}" 2>&1 || true)" ;;
    esac
    expected=valid
    [ "${want}" = valid ] || expected="refused ${rule}"
    if [ "${got}" = "${expected}" ]; then
        pass "vector ${vector}: ${expected}"
    elif reason="$(outside_floor "${vector}")" && [ -n "${reason}" ]; then
        OUTSIDE=$((OUTSIDE + 1))
        echo "OUTSIDE THE FLOOR: ${vector} ${reason} (got '${got}', the spec says '${expected}')"
    else
        fail "vector ${vector}: '${got}', want '${expected}'"
    fi
done <"${VECTORS}/expected.tsv"
echo "vectors: ${ROWS} rows read at $(sed -n 's/^COMMIT=//p' "${REPO_ROOT}/scripts/gate/vectors.pin"), ${OUTSIDE} outside this repository's floor"

# SET EQUALITY, BOTH DIRECTIONS: every vector file the canonical tree holds is
# named by its manifest, so an extra fixture cannot sit unrun and pass unseen.
# A pins/ vector is a DIRECTORY of locks and pins named as one row, and
# expected.tsv and derived-from.tsv are manifests rather than vectors, so the
# comparison is at the granularity the manifest names.
UNNAMED=0
while IFS= read -r vector; do
    grep -q "^${vector}${TAB}" "${VECTORS}/expected.tsv" || { UNNAMED=$((UNNAMED + 1)); fail "vector not named by expected.tsv: ${vector}"; }
done < <(cd "${VECTORS}" && find . -type f ! -name expected.tsv ! -name derived-from.tsv -printf '%P\n' |
    grep -v '^repos/' | sed -E 's#^(pins/[^/]+/[^/]+)/.*#\1#' | sort -u)
[ "${UNNAMED}" != 0 ] || pass "every vector file in the pinned tree is named by its manifest"

# And this repository's own pin is one of the forms the family describes.
expect_pin="$(bash "${CHECK}" vectors-pin "${REPO_ROOT}/scripts/gate/vectors.pin" 2>&1 || true)"
[ "${expect_pin}" = valid ] && pass "scripts/gate/vectors.pin is a valid mica-vectors-pin v1" ||
    fail "scripts/gate/vectors.pin: ${expect_pin}"

expect() { # <0|1> <case> <needle> <command>...
    local want="$1" name="$2" needle="$3" rc=0 out
    shift 3
    out="$("$@" 2>&1)" || rc=$?
    if { [ "${want}" = 0 ] && [ "${rc}" = 0 ]; } || { [ "${want}" = 1 ] && [ "${rc}" != 0 ]; }; then
        if [ "${out#*"${needle}"}" != "${out}" ]; then pass "${name}"; return; fi
    fi
    fail "${name}: wanted exit ${want} with \"${needle}\", got ${rc}: ${out}"
}

expect 0 "the committed locks are well formed" "is well formed" bash "${LOCKS}" check

# A fixture release of mica-build-env served from file://, and a locks/ taken from it.
RELEASE="$(awk -F"\t" '$1 == "release" { print $3 }' "${REPO_ROOT}/locks/mica-build-env.lock")"
publish() { # <dir> <lock file>: the release assets
    mkdir -p "$1"
    cp "$2" "$1/mica-build-env.lock"
    (cd "$1" && sha256sum mica-build-env.lock >SHA256SUMS)
}
consumer() { # <dir> <lock file> <trust>: a locks/ directory
    mkdir -p "$1/pins"
    cp "$2" "$1/mica-build-env.lock"
    cp "${REPO_ROOT}/locks/upstream.lock" "$1/upstream.lock"
    printf '# mica-pin v1\nREPOSITORY=mica-build-env\nRELEASE=%s\nSHA256SUMS=%s\n' "${RELEASE}" "$3" >"$1/pins/mica-build-env.pin"
}
verify() { MICA_LOCKS_DIR="$1" MICA_LOCKS_RELEASES="file://${T}/releases" bash "${LOCKS}" verify; }
D="${T}/releases/mica-build-env/releases/download/${RELEASE}"
publish "${D}" "${REPO_ROOT}/locks/mica-build-env.lock"
TRUST="$(sha256sum "${D}/SHA256SUMS" | cut -d' ' -f1)"

consumer "${T}/good" "${REPO_ROOT}/locks/mica-build-env.lock" "${TRUST}"
expect 0 "verify: a lock that is its release's asset" "verified" verify "${T}/good"
consumer "${T}/trust" "${REPO_ROOT}/locks/mica-build-env.lock" "$(printf 'f%.0s' {1..64})"
expect 1 "verify: a pin recording another SHA256SUMS hash" "hashes to" verify "${T}/trust"
consumer "${T}/altered" "${REPO_ROOT}/locks/mica-build-env.lock" "${TRUST}"
sed -i 's/^image\tupstream\tubuntu:24.04\tamd64/# removed/' "${T}/altered/mica-build-env.lock"
expect 1 "verify: a lock altered after the release" "does not name" verify "${T}/altered"
consumer "${T}/norelease" "${REPO_ROOT}/locks/mica-build-env.lock" "${TRUST}"
sed -i 's/^RELEASE=.*/RELEASE=20990101-0000/' "${T}/norelease/pins/mica-build-env.pin"
sed -i "s/^release\tmica-build-env\t${RELEASE}/release\tmica-build-env\t20990101-0000/" "${T}/norelease/mica-build-env.lock"
expect 1 "verify: a release that cannot be downloaded" "downloading SHA256SUMS" verify "${T}/norelease"
X="${T}/releases/mica-build-env/releases/download/20990101-0001"
publish "${X}" "${REPO_ROOT}/locks/mica-build-env.lock"
echo "0000000000000000000000000000000000000000000000000000000000000000  extra.tar.gz" >>"${X}/SHA256SUMS"
consumer "${T}/extra" "${REPO_ROOT}/locks/mica-build-env.lock" "$(sha256sum "${X}/SHA256SUMS" | cut -d' ' -f1)"
sed -i 's/^RELEASE=.*/RELEASE=20990101-0001/' "${T}/extra/pins/mica-build-env.pin"
sed -i "s/^release\tmica-build-env\t${RELEASE}/release\tmica-build-env\t20990101-0001/" "${T}/extra/mica-build-env.lock"
expect 1 "verify: a SHA256SUMS listing more than the lock" "rather than exactly" verify "${T}/extra"
consumer "${T}/offline" "${REPO_ROOT}/locks/mica-build-env.lock" "${TRUST}"
printf '# mica-pin v1\nREPOSITORY=mica-build-env\nRELEASE=offline\nSHA256SUMS=%s\nCHECKOUT=/srv/mica-build-env\n' "${TRUST}" >"${T}/offline/pins/mica-build-env.pin"
sed -i "s/^release\tmica-build-env\t${RELEASE}/release\tmica-build-env\toffline/; s#ghcr.io/micaoss/#local/#; /^image\tmica-build-env/s#ghcr.io/micaoss/#local/#" "${T}/offline/mica-build-env.lock"
sed -i "/^image\tmica-build-env/s#ghcr\.io/micaoss/#local/#" "${T}/offline/mica-build-env.lock"
expect 1 "check: an offline pin under CI" "checkout-in-ci" env CI=true MICA_LOCKS_DIR="${T}/offline" bash "${LOCKS}" check
consumer "${T}/nopin" "${REPO_ROOT}/locks/mica-build-env.lock" "${TRUST}"
rm "${T}/nopin/pins/mica-build-env.pin"
expect 1 "check: a lock without its pin" "lock-without-pin" env MICA_LOCKS_DIR="${T}/nopin" bash "${LOCKS}" check

expect 0 "from.sh: the rust index by digest" "ghcr.io/micaoss/mica-build-env:rust." bash "${FROM}" --ref rust
expect 0 "from.sh: the arm64 base manifest by digest" "ghcr.io/micaoss/mica-build-env@sha256:" bash "${FROM}" --arch=arm64 --ref base
expect 0 "from.sh: an upstream image by its original reference" "docker.io/moby/buildkit:v0.33.0@sha256:" bash "${FROM}" --upstream moby/buildkit:v0.33.0
expect 1 "from.sh: an image the lock does not list" "taken only from mica-build-env's upstream rows" bash "${FROM}" --upstream registry:2
expect 1 "from.sh: a mica-build-env image the lock does not name" "has no image row" bash "${FROM}" --ref python

echo "RESULT: ${PASS} passed, ${FAIL} failed"
[ "${FAIL}" = 0 ]
