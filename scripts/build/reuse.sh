#!/usr/bin/env bash
# The package-version guard (mica:docs/decisions/2026-09-15-package-versions.md
# R5): the archives of _out/debs/{amd64,arm64}/pool against the latest release
# of this repository, read anonymously. It prints one line per archive,
# "<arch> <package> <version> new|reused <sha256>", and refuses:
#   - a package at the version it was released at whose inputs hash
#     (_out/debs/<arch>/inputs.tsv, the pool layer's mica.inputs) changed;
#   - a package at the version it was released at whose bytes differ;
#   - a package at a lower version than it was released at;
#   - a release whose lock or pool manifest cannot be read back or does not
#     match its digest, or whose layers carry mica.inputs only in part.
# The previous release is the newest one whose pool layers carry mica.inputs;
# older releases predate the rules and are passed over. With none, every
# package is new.
#
#   bash scripts/build/reuse.sh [--exclude <release>]
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ARCHES=(amd64 arm64)
die() { echo "reuse.sh: error: $*" >&2; exit 1; }
for t in git curl jq sha256sum dpkg dpkg-deb; do command -v "${t}" >/dev/null 2>&1 || die "${t} is required"; done

EXCLUDE=""
case "${1:-}" in
"") ;;
--exclude) EXCLUDE="${2:?--exclude takes a release}" ;;
*) die "usage: bash scripts/build/reuse.sh [--exclude <release>]" ;;
esac

cd "${REPO_ROOT}"
ORIGIN="$(git remote get-url origin)"
[[ "${ORIGIN}" =~ github\.com[:/]([A-Za-z0-9._-]+)/([A-Za-z0-9._-]+)$ ]] || die "origin ${ORIGIN} is not a GitHub repository"
SLUG="${BASH_REMATCH[1]}/${BASH_REMATCH[2]%.git}"
REPOSITORY="${SLUG#*/}"
LOCK="${REPOSITORY}.lock"
GIT_URL="${MICA_RELEASE_GIT:-https://github.com/${SLUG}.git}"
DOWNLOAD="${MICA_RELEASE_DOWNLOAD:-https://github.com/${SLUG}/releases/download}"
REGISTRY="${MICA_RELEASE_REGISTRY:-https://ghcr.io}"
OCI_NAME="$(printf '%s' "${SLUG}" | tr '[:upper:]' '[:lower:]')"

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

# The current archives: package, version, sha256 and its producer's inputs hash.
declare -A PRODUCER_OF=()
while read -r producer _dir packages _enablement; do
    for p in ${packages//,/ }; do PRODUCER_OF["${p}"]="${producer}"; done
done < <(bash scripts/deb/producers.sh)
: >"${WORK}/current.tsv"
for arch in "${ARCHES[@]}"; do
    inputs="_out/debs/${arch}/inputs.tsv"
    [ -f "${inputs}" ] || die "${inputs} does not exist; the pool was not built by scripts/deb/build.sh"
    for deb in "_out/debs/${arch}/pool/"*.deb; do
        [ -f "${deb}" ] || die "_out/debs/${arch}/pool holds no archive"
        name="$(dpkg-deb --field "${deb}" Package)"
        hash="$(awk -F'\t' -v p="${PRODUCER_OF[${name}]:-}" '$1 == p { print $2 }' "${inputs}")"
        [[ "${hash}" =~ ^[0-9a-f]{64}$ ]] || die "${inputs} records no inputs hash for ${PRODUCER_OF[${name}]:-the producer of ${name}}"
        printf '%s\t%s\t%s\t%s\t%s\n' "${arch}" "${name}" "$(dpkg-deb --field "${deb}" Version)" \
            "$(sha256sum "${deb}" | cut -d' ' -f1)" "${hash}" >>"${WORK}/current.tsv"
    done
done

# The previous release under the rules: the newest YYYYMMDD-HHMM release (the
# excluded one aside) whose pool layers carry mica.inputs. A release whose
# layers carry none predates the rules and is passed over; one that cannot be
# read back, or whose layers carry mica.inputs only in part, is refused.
fetch() { curl -fsSL --retry 3 -o "$2" "$1" || die "$1 cannot be read anonymously"; }
challenge="$(curl -sS -o /dev/null -D - "${REGISTRY}/v2/" | tr -d '\r' | sed -n 's/^[Ww][Ww][Ww]-[Aa]uthenticate: *//p')"
realm="$(printf '%s' "${challenge}" | sed -n 's/.*realm="\([^"]*\)".*/\1/p')"
service="$(printf '%s' "${challenge}" | sed -n 's/.*service="\([^"]*\)".*/\1/p')"
[ -n "${realm}" ] || die "${REGISTRY} names no token realm"
token="$(curl -fsS -G --data-urlencode "service=${service}" --data-urlencode "scope=repository:${OCI_NAME}:pull" "${realm}" |
    jq -r '.token // .access_token // empty')" || die "no anonymous pull token for ${OCI_NAME}"

# read_release <release>: its packages into ${WORK}/<release>.tsv (arch, package,
# version, sha256, mica.inputs); prints how many of its layers carry mica.inputs
# and how many do not.
read_release() {
    local release="$1" dir="${WORK}/$1" arch reference digest kind name parch version sha inputs
    mkdir -p "${dir}"
    fetch "${DOWNLOAD}/${release}/SHA256SUMS" "${dir}/SHA256SUMS"
    fetch "${DOWNLOAD}/${release}/${LOCK}" "${dir}/${LOCK}"
    [ "$(sed 's/^[0-9a-f]\{64\}  //' "${dir}/SHA256SUMS")" = "${LOCK}" ] || die "SHA256SUMS of ${release} does not list exactly ${LOCK}"
    (cd "${dir}" && sha256sum --quiet -c SHA256SUMS) || die "${LOCK} of ${release} does not match its SHA256SUMS"
    result="$(bash scripts/build/check-lock.sh lock "${dir}/${LOCK}")" || die "${LOCK} of ${release} is ${result}"
    : >"${WORK}/${release}.tsv"
    for arch in "${ARCHES[@]}"; do
        reference="$(awk -F'\t' -v a="${arch}" '$1 == "pool" && $2 == a { print $3 }' "${dir}/${LOCK}")"
        [ -n "${reference}" ] || continue
        digest="${reference##*@}"
        curl -fsS -H "Authorization: Bearer ${token}" -H "Accept: application/vnd.oci.image.manifest.v1+json" \
            -o "${dir}/manifest.${arch}" "${REGISTRY}/v2/${OCI_NAME}/manifests/${digest}" ||
            die "the ${arch} pool of ${release} (${digest}) cannot be read anonymously"
        [ "sha256:$(sha256sum "${dir}/manifest.${arch}" | cut -d' ' -f1)" = "${digest}" ] ||
            die "the ${arch} pool of ${release} reads as another manifest than ${digest}"
        while IFS=$'\t' read -r kind name parch version sha; do
            [ "${kind}" = package ] && [ "${parch}" = "${arch}" ] || continue
            inputs="$(jq -r --arg d "sha256:${sha}" '[.layers[] | select(.digest == $d)] | if length == 1 then (.[0].annotations["mica.inputs"] // "-") else "missing" end' "${dir}/manifest.${arch}")"
            [ "${inputs}" != missing ] || die "${name} ${arch} of ${release} is not a layer of its pool"
            printf '%s\t%s\t%s\t%s\t%s\n' "${arch}" "${name}" "${version}" "${sha}" "${inputs}" >>"${WORK}/${release}.tsv"
        done <"${dir}/${LOCK}"
    done
    printf '%s %s\n' "$(awk -F'\t' '$5 != "-"' "${WORK}/${release}.tsv" | grep -c .)" "$(awk -F'\t' '$5 == "-"' "${WORK}/${release}.tsv" | grep -c .)"
}

PREVIOUS=""
while IFS= read -r release; do
    read_release "${release}" >"${WORK}/counts"
    read -r carrying lacking <"${WORK}/counts"
    if [ "${carrying}" -gt 0 ] && [ "${lacking}" -gt 0 ]; then
        die "${release} carries mica.inputs on ${carrying} layer(s) and lacks it on ${lacking}"
    elif [ "${carrying}" -gt 0 ]; then
        PREVIOUS="${release}"
        break
    fi
    echo "reuse.sh: ${release} predates the package-version rules (no mica.inputs); passed over" >&2
done < <(git ls-remote --tags "${GIT_URL}" | awk '{ sub("refs/tags/", "", $2); sub("\\^\\{\\}$", "", $2); print $2 }' |
    grep -E '^[0-9]{8}-[0-9]{4}$' | grep -vxF "${EXCLUDE:-none}" | LC_ALL=C sort -ru || true)
if [ -z "${PREVIOUS}" ]; then
    echo "reuse.sh: no release of ${SLUG} under the package-version rules; every package is new" >&2
    awk -F'\t' '{ printf "%s %s %s new %s\n", $1, $2, $3, $4 }' "${WORK}/current.tsv"
    exit 0
fi
cp "${WORK}/${PREVIOUS}.tsv" "${WORK}/previous.tsv"

refusals=0
while IFS=$'\t' read -r arch name version sha hash; do
    read -r _a _n pversion psha pinputs < <(awk -F'\t' -v a="${arch}" -v n="${name}" '$1 == a && $2 == n { print $1, $2, $3, $4, $5 }' "${WORK}/previous.tsv") || true
    if [ -z "${pversion:-}" ] || dpkg --compare-versions "${version}" gt "${pversion}"; then
        printf '%s %s %s new %s\n' "${arch}" "${name}" "${version}" "${sha}"
    elif [ "${version}" != "${pversion}" ]; then
        echo "reuse.sh: ${name} ${arch} is at ${version}, lower than ${pversion} in ${PREVIOUS}" >&2
        refusals=$((refusals + 1))
    elif [ "${pinputs}" != "${hash}" ]; then
        echo "reuse.sh: inputs of ${name} changed without a version bump (${arch} ${version}: ${pinputs} in ${PREVIOUS}, ${hash} now)" >&2
        refusals=$((refusals + 1))
    elif [ "${psha}" != "${sha}" ]; then
        echo "reuse.sh: ${name} ${arch} ${version} does not rebuild byte-identically to ${PREVIOUS} (${psha} released, ${sha} built); bump its version" >&2
        refusals=$((refusals + 1))
    else
        printf '%s %s %s reused %s\n' "${arch}" "${name}" "${version}" "${sha}"
    fi
    pversion="" psha="" pinputs=""
done <"${WORK}/current.tsv"
[ "${refusals}" = 0 ] || die "${refusals} package(s) refused against ${PREVIOUS}"
echo "reuse.sh: checked against ${PREVIOUS}" >&2
