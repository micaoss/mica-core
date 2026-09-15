#!/usr/bin/env bash
# The package gate over _out/debs/<arch>/pool.
#
#   bash scripts/deb/package-gate.sh [--arch <amd64|arm64>] [--no-reproduce]
#
# Per architecture, read out of the archives with dpkg-deb in the mica-build-env base image:
#   - the pool holds exactly the packages the producers declare, all
#     Architecture <arch>, all with one <VERSION>+git<commit12>[.dirty]-1 stamp;
#   - no Replaces, and no non-directory path shipped by two packages;
#   - a dependency on a package built here names its exact pool version;
#   - a non-empty /usr/share/doc/<package>/copyright;
#   - as many multi-user.target.wants links as ENABLEMENT declares;
#   - no DEBIAN/conffiles, and maintainer scripts that parse as POSIX sh;
# and, unless --no-reproduce, one producer rebuilt on an empty-cache builder
# gives byte-identical archives. --arch limits the gate to one pool.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DIST="${REPO_ROOT}/_out/debs"
die() { echo "package-gate.sh: error: $*" >&2; exit 1; }
command -v docker >/dev/null 2>&1 || die "docker is required"

ARCHES=(amd64 arm64)
REPRODUCE=1
while [ "$#" -gt 0 ]; do
    case "$1" in
    --arch)
        case "${2-}" in amd64 | arm64) ARCHES=("$2") ;; *) die "--arch takes amd64 or arm64" ;; esac
        shift 2
        ;;
    --no-reproduce) REPRODUCE=0; shift ;;
    *) die "usage: bash scripts/deb/package-gate.sh [--arch <amd64|arm64>] [--no-reproduce]" ;;
    esac
done
for arch in "${ARCHES[@]}"; do
    [ -d "${DIST}/${arch}/pool" ] || die "${DIST}/${arch}/pool does not exist; build it with make deb MICA_ARCH=${arch}"
done

mapfile -t ROWS < <(bash "${REPO_ROOT}/scripts/deb/producers.sh")
case "$(uname -m)" in
x86_64) HOST_ARCH=amd64 ;;
aarch64 | arm64) HOST_ARCH=arm64 ;;
*) die "$(uname -m) is not amd64 or arm64" ;;
esac
IMAGE="$(bash "${REPO_ROOT}/scripts/build/from.sh" --arch="${HOST_ARCH}" --ref base)"

mkdir -p "${REPO_ROOT}/tmp"
WORK="$(mktemp -d "${REPO_ROOT}/tmp/package-gate.XXXXXX")"
BUILDERS=()
cleanup() {
    for b in ${BUILDERS[@]+"${BUILDERS[@]}"}; do docker buildx rm "${b}" >/dev/null 2>&1 || true; done
    rm -rf "${WORK}"
}
trap cleanup EXIT
printf '%s\n' "${ROWS[@]}" >"${WORK}/producers.tsv"

LOG="${WORK}/static.log"
status=0
docker run --rm -i --label ai-agent=true \
    -v "${DIST}:/dist:ro" -v "${WORK}/producers.tsv:/producers.tsv:ro" \
    --entrypoint /bin/bash "${IMAGE}" -s "${ARCHES[@]}" <<'INNER' 2>&1 | tee "${LOG}" || status=1
set -euo pipefail
PASS=0
FAIL=0
pass() { PASS=$((PASS + 1)); echo "PASS: $1"; }
fail() { FAIL=$((FAIL + 1)); echo "FAIL: $1"; }

declare -A WANTS=()
LOCAL=" "
while read -r _producer _dir packages enablement; do
    for p in ${packages//,/ }; do LOCAL="${LOCAL}${p} "; done
    for e in ${enablement//,/ }; do WANTS["${e%%=*}"]="${e#*=}"; done
done </producers.tsv
EXPECTED="$(printf '%s\n' ${LOCAL} | LC_ALL=C sort | tr '\n' ' ')"

STAMPS=" "
ARCHIVES=0
for arch in "$@"; do
    pool="/dist/${arch}/pool"
    mapfile -t debs < <(find "${pool}" -maxdepth 1 -type f -name '*.deb' | LC_ALL=C sort)
    ARCHIVES=$((ARCHIVES + ${#debs[@]}))
    declare -A VER=()
    for deb in "${debs[@]}"; do VER["$(dpkg-deb --field "${deb}" Package)"]="$(dpkg-deb --field "${deb}" Version)"; done
    got="$(printf '%s\n' "${!VER[@]}" | LC_ALL=C sort | tr '\n' ' ')"
    if [ "${got}" = "${EXPECTED}" ]; then pass "${arch}: the pool holds exactly ${got% }"; else fail "${arch}: the pool holds [${got% }], the producers declare [${EXPECTED% }]"; fi

    declare -A OWNER=()
    for deb in "${debs[@]}"; do
        name="$(dpkg-deb --field "${deb}" Package)"
        version="${VER[${name}]}"

        a="$(dpkg-deb --field "${deb}" Architecture)"
        if [ "${a}" = "${arch}" ]; then pass "${name} ${arch}: Architecture ${a}"; else fail "${name} ${arch}: Architecture ${a}"; fi

        stamp="${version##*+}"
        if [[ "${stamp}" =~ ^git[0-9a-f]{12}(\.dirty)?-[0-9]+$ ]]; then
            STAMPS="${STAMPS}${stamp} "
        else
            fail "${name} ${arch}: version ${version} carries no git<commit12>-<rev> stamp"
        fi

        if [ -z "$(dpkg-deb --field "${deb}" Replaces)" ]; then pass "${name} ${arch}: no Replaces"; else fail "${name} ${arch}: declares Replaces"; fi

        IFS=',' read -ra entries <<<"$(dpkg-deb --field "${deb}" Depends)"
        for entry in ${entries[@]+"${entries[@]}"}; do
            IFS='|' read -ra alts <<<"${entry}"
            for alt in "${alts[@]}"; do
                read -r dep _rest <<<"${alt}"
                dep="${dep%%(*}"
                case "${LOCAL}" in *" ${dep} "*) ;; *) continue ;; esac
                case "${alt}" in
                *"(= ${VER[${dep}]:-<absent>})"*) pass "${name} ${arch}: depends on ${dep} (= ${VER[${dep}]})" ;;
                *) fail "${name} ${arch}: depends on ${dep} as '${alt# }', not its exact pool version ${VER[${dep}]:-<absent>}" ;;
                esac
            done
        done

        listing="$(dpkg-deb --contents "${deb}")"
        dup=""
        while read -r mode _own _size _date _time path _rest; do
            case "${mode}" in d* | '') continue ;; esac
            path="${path#./}"
            if [ -n "${OWNER[${path}]:-}" ]; then dup="${dup} /${path} (${OWNER[${path}]})"; else OWNER["${path}"]="${name}"; fi
        done <<<"${listing}"
        if [ -z "${dup}" ]; then pass "${name} ${arch}: shares no path with another package"; else fail "${name} ${arch}: ships paths another package owns:${dup}"; fi

        size="$(awk -v p="./usr/share/doc/${name}/copyright" '$6 == p { print $3; exit }' <<<"${listing}")"
        if [ "${size:-0}" -gt 0 ]; then pass "${name} ${arch}: copyright (${size} bytes)"; else fail "${name} ${arch}: no non-empty /usr/share/doc/${name}/copyright"; fi

        links="$(awk '$1 ~ /^l/ && $6 ~ /^\.\/etc\/systemd\/system\/multi-user\.target\.wants\// { n++ } END { print n + 0 }' <<<"${listing}")"
        if [ "${links}" = "${WANTS[${name}]:-}" ]; then
            pass "${name} ${arch}: ${links} multi-user.target.wants link(s)"
        else
            fail "${name} ${arch}: ${links} multi-user.target.wants link(s), ENABLEMENT declares ${WANTS[${name}]:-none}"
        fi

        ctl="/tmp/ctl/${arch}/${name}"
        mkdir -p "${ctl}"
        dpkg-deb --control "${deb}" "${ctl}"
        if [ ! -e "${ctl}/conffiles" ]; then pass "${name} ${arch}: no conffiles"; else fail "${name} ${arch}: carries DEBIAN/conffiles"; fi
        for s in preinst postinst prerm postrm; do
            [ -f "${ctl}/${s}" ] || continue
            if err="$(sh -n "${ctl}/${s}" 2>&1)"; then pass "${name} ${arch}: ${s} parses as POSIX sh"; else fail "${name} ${arch}: ${s} is not valid POSIX sh: ${err}"; fi
        done
    done
    unset VER OWNER
done
n="$(printf '%s\n' ${STAMPS} | LC_ALL=C sort -u | grep -c .)"
if [ "${n}" = 1 ]; then pass "one git stamp across every archive ($(printf '%s\n' ${STAMPS} | sort -u))"; else fail "the archives carry ${n} git stamps:${STAMPS% }"; fi
echo "GATE ${PASS} ${FAIL} ${ARCHIVES}"
INNER

read -r PASS FAIL ARCHIVES < <(sed -n 's/^GATE //p' "${LOG}") || true
[ -n "${PASS:-}" ] || die "the gate container reported no result"
if [ "${status}" != 0 ] || [ "${FAIL}" != 0 ]; then
    echo "RESULT: FAIL (${PASS}/$((PASS + FAIL)) checks, ${ARCHIVES} archives)"
    exit 1
fi
if [ "${REPRODUCE}" = 0 ]; then
    echo "RESULT: PASS (${PASS}/${PASS} checks, ${ARCHIVES} archives, no rebuild)"
    exit 0
fi

# Rebuild one producer per architecture on an empty-cache builder and compare.
COMPARED=0
for i in "${!ARCHES[@]}"; do
    arch="${ARCHES[${i}]}"
    read -r producer _dir packages _enablement <<<"${ROWS[$((i % ${#ROWS[@]}))]}"
    pool="${DIST}/${arch}/pool"
    mkdir -p "${WORK}/before/${arch}"
    names=()
    for p in ${packages//,/ }; do
        f="$(find "${pool}" -maxdepth 1 -name "${p}_*_${arch}.deb" -printf '%f\n')"
        [ -n "${f}" ] || die "${pool} has no ${p} archive to compare"
        cp "${pool}/${f}" "${WORK}/before/${arch}/"
        names+=("${f}")
    done
    builder="mica-gate-${arch}-$$"
    docker buildx create --name "${builder}" --driver docker-container \
        --driver-opt "image=$(bash "${REPO_ROOT}/scripts/build/from.sh" --upstream moby/buildkit:v0.33.0)" >/dev/null
    BUILDERS+=("${builder}")
    log="${WORK}/rebuild-${arch}.log"
    echo "package-gate.sh: rebuilding ${producer} for ${arch} on ${builder}"
    if ! BUILDX_BUILDER="${builder}" BUILDKIT_PROGRESS=plain bash "${REPO_ROOT}/scripts/deb/build.sh" --producer "${producer}" --arch "${arch}" >"${log}" 2>&1; then
        tail -n 30 "${log}"
        FAIL=$((FAIL + 1))
        echo "FAIL: ${arch}: the rebuild of ${producer} failed"
        continue
    fi
    for n in "${names[@]}"; do
        COMPARED=$((COMPARED + 1))
        if ! grep -c "pack\.sh: ${n} " "${log}" >/dev/null; then
            FAIL=$((FAIL + 1))
            echo "FAIL: ${arch}: the rebuild did not run pack.sh for ${n} (a cached layer was replayed)"
        elif cmp -s "${WORK}/before/${arch}/${n}" "${pool}/${n}"; then
            PASS=$((PASS + 1))
            echo "PASS: ${arch}: ${n} is byte-identical when rebuilt"
        else
            FAIL=$((FAIL + 1))
            echo "FAIL: ${arch}: ${n} differs when rebuilt"
        fi
    done
done
echo "RESULT: $([ "${FAIL}" = 0 ] && echo PASS || echo FAIL) (${PASS}/$((PASS + FAIL)) checks, ${ARCHIVES} archives, ${COMPARED} rebuilt archives compared)"
[ "${FAIL}" = 0 ]
