#!/usr/bin/env bash
# The package gate over _out/debs/<arch>/pool.
#
#   bash scripts/deb/package-gate.sh [--arch <amd64|arm64>] [--no-reproduce]
#
# `mica-tools pool gate` answers what the archives answer by themselves, and
# holds every pool item to its name and version. A core component is two pool
# items, <name>_<version>_<arch>.core.img and .core.json
# (mica-build-tools:docs/spec/release-lock.md 1.2.7), and what they are is
# checked here: the tools know no type. Then, per architecture, what needs this
# repository's producers:
#   - read out of the archives with dpkg-deb in the mica-build-env base image:
#     the pool holds exactly the Debian packages the producers declare, each at
#     its producer's declared version; a dependency on a package built here
#     names its exact pool version; as many multi-user.target.wants links as
#     ENABLEMENT declares;
#   - read out of the core components with squashfs-tools and cryptsetup in the
#     pinned alpine image: exactly the declared core components at their
#     declared versions and no other item; each record an unsigned mica/core/v1
#     of its package, version and architecture, naming its image by length and
#     sha256; nothing outside /usr and /etc, as many multi-user.target.wants
#     links as ENABLEMENT declares, and a hash tree that verifies against the
#     record's root hash;
# and, unless --no-reproduce, one producer rebuilt on an empty-cache builder
# gives byte-identical files -- on the first architecture the producer of the
# core components. --arch limits the gate to one pool.
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
"${REPO_ROOT}/bin/mica-tools" pool gate $(printf -- '--arch %s ' "${ARCHES[@]}") || die "mica-tools pool gate refused the pool"
case "$(uname -m)" in
x86_64) HOST_ARCH=amd64 ;;
aarch64 | arm64) HOST_ARCH=arm64 ;;
*) die "$(uname -m) is not amd64 or arm64" ;;
esac
IMAGE="$("${REPO_ROOT}/bin/mica-tools" from --ref "mica-build-env:base@${HOST_ARCH}")"

mkdir -p "${REPO_ROOT}/tmp"
WORK="$(mktemp -d "${REPO_ROOT}/tmp/package-gate.XXXXXX")"
BUILDERS=()
cleanup() {
    for b in ${BUILDERS[@]+"${BUILDERS[@]}"}; do docker buildx rm "${b}" >/dev/null 2>&1 || true; done
    rm -rf "${WORK}"
}
trap cleanup EXIT
: >"${WORK}/core.tsv"
for row in "${ROWS[@]}"; do
    read -r producer _dir packages enablement <<<"${row}"
    version="$(bash "${REPO_ROOT}/scripts/deb/producers.sh" --version-of "${producer}")"
    for p in ${packages//,/ }; do
        if [ "$(bash "${REPO_ROOT}/scripts/deb/producers.sh" --kind-of "${p}")" = core ]; then
            links="$(printf '%s\n' ${enablement//,/ } | sed -n "s/^${p}=//p")"
            printf '%s %s %s\n' "${p}" "${version}" "${links}" >>"${WORK}/core.tsv"
        else
            printf '%s %s\n' "${p}" "${version}"
        fi
    done
done >"${WORK}/versions.tsv"
# The Debian side reads only the Debian packages' rows.
while read -r producer dir packages enablement; do
    kept=()
    for p in ${packages//,/ }; do
        grep -q "^${p} " "${WORK}/core.tsv" || kept+=("${p}")
    done
    [ "${#kept[@]}" -eq 0 ] || printf '%s %s %s %s\n' "${producer}" "${dir}" "$(IFS=,; echo "${kept[*]}")" "${enablement}"
done < <(printf '%s\n' "${ROWS[@]}") >"${WORK}/producers.tsv"

LOG="${WORK}/static.log"
status=0
docker run --rm -i --label ai-agent=true \
    -v "${DIST}:/dist:ro" -v "${WORK}/producers.tsv:/producers.tsv:ro" -v "${WORK}/versions.tsv:/versions.tsv:ro" \
    --entrypoint /bin/bash "${IMAGE}" -s "${ARCHES[@]}" <<'INNER' 2>&1 | tee "${LOG}" || status=1
set -euo pipefail
PASS=0
FAIL=0
pass() { PASS=$((PASS + 1)); echo "PASS: $1"; }
fail() { FAIL=$((FAIL + 1)); echo "FAIL: $1"; }

declare -A WANTS=() DECLARED=()
LOCAL=" "
while read -r _producer _dir packages enablement; do
    for p in ${packages//,/ }; do LOCAL="${LOCAL}${p} "; done
    for e in ${enablement//,/ }; do WANTS["${e%%=*}"]="${e#*=}"; done
done </producers.tsv
EXPECTED="$(printf '%s\n' ${LOCAL} | LC_ALL=C sort | tr '\n' ' ')"
while read -r p v; do DECLARED["${p}"]="${v}"; done </versions.tsv

ARCHIVES=0
for arch in "$@"; do
    pool="/dist/${arch}/pool"
    mapfile -t debs < <(find "${pool}" -maxdepth 1 -type f -name '*.deb' | LC_ALL=C sort)
    ARCHIVES=$((ARCHIVES + ${#debs[@]}))
    declare -A VER=()
    for deb in "${debs[@]}"; do VER["$(dpkg-deb --field "${deb}" Package)"]="$(dpkg-deb --field "${deb}" Version)"; done
    got="$(printf '%s\n' "${!VER[@]}" | LC_ALL=C sort | tr '\n' ' ')"
    if [ "${got}" = "${EXPECTED}" ]; then pass "${arch}: the pool holds exactly ${got% }"; else fail "${arch}: the pool holds [${got% }], the producers declare [${EXPECTED% }]"; fi

    for deb in "${debs[@]}"; do
        name="$(dpkg-deb --field "${deb}" Package)"
        version="${VER[${name}]}"

        if [ "${version}" = "${DECLARED[${name}]:-}" ]; then pass "${name} ${arch}: version ${version} is its producer's declared version"; else fail "${name} ${arch}: version ${version}, its producer declares ${DECLARED[${name}]:-none}"; fi

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
        links="$(awk '$1 ~ /^l/ && $6 ~ /^\.\/etc\/systemd\/system\/multi-user\.target\.wants\// { n++ } END { print n + 0 }' <<<"${listing}")"
        if [ "${links}" = "${WANTS[${name}]:-}" ]; then
            pass "${name} ${arch}: ${links} multi-user.target.wants link(s)"
        else
            fail "${name} ${arch}: ${links} multi-user.target.wants link(s), ENABLEMENT declares ${WANTS[${name}]:-none}"
        fi
    done
    unset VER
done
echo "GATE ${PASS} ${FAIL} ${ARCHIVES}"
INNER

read -r PASS FAIL ARCHIVES < <(sed -n 's/^GATE //p' "${LOG}") || true
[ -n "${PASS:-}" ] || die "the gate container reported no result"

# The core components, in the pinned alpine image.
CORE_TOOLS="$("${REPO_ROOT}/bin/mica-tools" from --ref upstream:alpine:3.24.1)"
CORE_LOG="${WORK}/core.log"
docker run --rm -i --label ai-agent=true \
    -v "${DIST}:/dist:ro" -v "${WORK}/core.tsv:/core.tsv:ro" \
    --entrypoint /bin/sh "${CORE_TOOLS}" -s "${ARCHES[@]}" <<'INNER' 2>&1 | tee "${CORE_LOG}" || status=1
set -eu
apk add --no-cache -q squashfs-tools cryptsetup jq >/dev/null
PASS=0 FAIL=0 COUNT=0
pass() { PASS=$((PASS + 1)); echo "PASS: $1"; }
fail() { FAIL=$((FAIL + 1)); echo "FAIL: $1"; }
for arch in "$@"; do
    pool="/dist/${arch}/pool"
    want="$(cut -d' ' -f1 /core.tsv | LC_ALL=C sort | tr '\n' ' ')"
    got="$(find "${pool}" -maxdepth 1 -name '*.core.json' -exec basename {} \; | sed 's/_.*//' | LC_ALL=C sort | tr '\n' ' ')"
    if [ "${got}" = "${want}" ]; then pass "${arch}: the core components are exactly ${want% }"; else fail "${arch}: the core components are [${got% }], the producers declare [${want% }]"; fi
    while read -r name version links; do
        record="${pool}/${name}_${version}_${arch}.core.json" image="${pool}/${name}_${version}_${arch}.core.img"
        if [ ! -f "${record}" ] || [ ! -f "${image}" ]; then fail "${name} ${arch}: no core component at its declared version ${version}"; continue; fi
        COUNT=$((COUNT + 1))
        pass "${name} ${arch}: version ${version} is its producer's declared version"
        # The record is the unsigned mica/core/v1 of this component: mica-build signs it and gives it its id.
        if jq -e --arg name "${name}" --arg version "${version}" --arg arch "${arch}" \
            '.schema == "mica/core/v1" and .package == $name and .version == $version and .arch == $arch
             and (has("id") | not) and (.content | has("signature") | not)' "${record}" >/dev/null 2>&1; then
            pass "${name} ${arch}: the record is an unsigned mica/core/v1 of ${name} ${version} ${arch}"
        else
            fail "${name} ${arch}: the record is not an unsigned mica/core/v1 of ${name} ${version} ${arch}"
        fi
        if [ "$(jq -r '.content.image.bytes' "${record}")" = "$(stat -c %s "${image}")" ] &&
            [ "$(jq -r '.content.image.sha256' "${record}")" = "$(sha256sum "${image}" | cut -d' ' -f1)" ]; then
            pass "${name} ${arch}: the record names its image by length and sha256"
        else
            fail "${name} ${arch}: the image is not the one its record names"
        fi
        listing="$(unsquashfs -lls -d '' "${image}" 2>/dev/null)"
        outside="$(printf '%s\n' "${listing}" | awk '{print $6}' | grep -v -E '^/?$|^/(usr|etc)(/|$)' || true)"
        if [ -z "${outside}" ]; then pass "${name} ${arch}: only /usr and /etc"; else fail "${name} ${arch}: holds $(printf '%s' "${outside}" | awk 'NR == 1') outside /usr and /etc"; fi
        count="$(printf '%s\n' "${listing}" | awk '$1 ~ /^l/ && $6 ~ /^\/etc\/systemd\/system\/multi-user\.target\.wants\// { n++ } END { print n + 0 }')"
        if [ "${count}" = "${links}" ]; then pass "${name} ${arch}: ${count} multi-user.target.wants link(s)"; else fail "${name} ${arch}: ${count} multi-user.target.wants link(s), ENABLEMENT declares ${links}"; fi
        offset="$(jq -r '.content.verity.hashOffset' "${record}")"
        if veritysetup verify "${image}" "${image}" "$(jq -r '.content.rootHash' "${record}")" --no-superblock --format 1 \
            --hash sha256 --data-block-size 4096 --hash-block-size 4096 --data-blocks "$(jq -r '.content.verity.dataBlocks' "${record}")" \
            --hash-offset "${offset}" --salt "$(jq -r '.content.verity.salt' "${record}")" >/dev/null 2>&1; then
            pass "${name} ${arch}: the hash tree verifies against the record's root hash"
        else
            fail "${name} ${arch}: the hash tree does not verify against the record's root hash"
        fi
    done </core.tsv
    # Nothing else is in the pool: an archive, or the two items of a declared component.
    stray="$(find "${pool}" -maxdepth 1 -type f ! -name '*.deb' -exec basename {} \; | LC_ALL=C sort | while read -r f; do
        while read -r name version _links; do
            case "${f}" in "${name}_${version}_${arch}.core.img" | "${name}_${version}_${arch}.core.json") continue 2 ;; esac
        done </core.tsv
        printf '%s ' "${f}"
    done)"
    if [ -z "${stray}" ]; then pass "${arch}: every item of the pool is a declared core component's"; else fail "${arch}: the pool holds ${stray% }, which no producer declares"; fi
done
echo "CORE ${PASS} ${FAIL} ${COUNT}"
INNER
read -r CORE_PASS CORE_FAIL CORE_COUNT < <(sed -n 's/^CORE //p' "${CORE_LOG}") || true
[ -n "${CORE_PASS:-}" ] || die "the core component container reported no result"
PASS=$((PASS + CORE_PASS)) FAIL=$((FAIL + CORE_FAIL)) ARCHIVES=$((ARCHIVES + CORE_COUNT))
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
    row="${ROWS[$((i % ${#ROWS[@]}))]}"
    # The first architecture rebuilds the core components' producer.
    if [ "${i}" = 0 ] && [ -s "${WORK}/core.tsv" ]; then
        first_core="$(head -n1 "${WORK}/core.tsv" | cut -d' ' -f1)"
        for candidate in "${ROWS[@]}"; do
            case ",$(cut -d' ' -f3 <<<"${candidate}")," in *",${first_core},"*) row="${candidate}" ;; esac
        done
    fi
    read -r producer _dir packages _enablement <<<"${row}"
    pool="${DIST}/${arch}/pool"
    mkdir -p "${WORK}/before/${arch}"
    names=()
    for p in ${packages//,/ }; do
        if [ "$(bash "${REPO_ROOT}/scripts/deb/producers.sh" --kind-of "${p}")" = core ]; then
            files="$(find "${pool}" -maxdepth 1 \( -name "${p}_*_${arch}.core.img" -o -name "${p}_*_${arch}.core.json" \) -printf '%f\n')"
        else
            files="$(find "${pool}" -maxdepth 1 -name "${p}_*_${arch}.deb" -printf '%f\n')"
        fi
        [ -n "${files}" ] || die "${pool} has no ${p} to compare"
        for f in ${files}; do
            cp "${pool}/${f}" "${WORK}/before/${arch}/"
            names+=("${f}")
        done
    done
    builder="mica-gate-${arch}-$$"
    docker buildx create --name "${builder}" --driver docker-container \
        --driver-opt "image=$("${REPO_ROOT}/bin/mica-tools" from --ref upstream:moby/buildkit:v0.33.0)" >/dev/null
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
        packed="/out/${n}\$"
        case "${n}" in *.core.img | *.core.json) packed=" ${n%%_*} .*: [0-9]* bytes, root hash" ;; esac
        if ! grep -c -- "${packed}" "${log}" >/dev/null; then
            FAIL=$((FAIL + 1))
            echo "FAIL: ${arch}: the rebuild did not pack ${n} (a cached layer was replayed)"
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
