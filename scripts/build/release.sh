#!/usr/bin/env bash
# Publish _out/debs/{amd64,arm64}/pool of a clean HEAD for the published GitHub
# Release <tag> of this repository:
#   - the OCI pools ghcr.io/micaoss/<repository>:pool.<arch>.<tag>, one manifest
#     per architecture with one layer per archive, read back anonymously. Every
#     package is at its producer's declared version and passes scripts/build/reuse.sh
#     against the previous release, so an unchanged pool is the same manifest
#     under a new tag;
#   - then the release assets <repository>.lock (release, pool and package rows)
#     and SHA256SUMS listing only it, read back anonymously.
# Nothing published is ever replaced.
#
#   GH_TOKEN=<token with contents:write and packages:write> bash scripts/build/release.sh <YYYYMMDD-HHMM>
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ARCHES=(amd64 arm64)
die() { echo "release.sh: error: $*" >&2; exit 1; }
for t in gh git curl jq sha256sum dpkg-deb date; do
    command -v "${t}" >/dev/null 2>&1 || die "${t} is required and not on PATH"
done

[ "$#" -eq 1 ] || die "usage: bash scripts/build/release.sh <YYYYMMDD-HHMM>"
TAG="$1"
[[ "${TAG}" =~ ^[0-9]{8}-[0-9]{4}$ ]] &&
    [ "$(date -u -d "${TAG:0:4}-${TAG:4:2}-${TAG:6:2} ${TAG:9:2}:${TAG:11:2}" +%Y%m%d-%H%M 2>/dev/null || true)" = "${TAG}" ] ||
    die "the tag '${TAG}' is not a UTC time YYYYMMDD-HHMM"

cd "${REPO_ROOT}"
[ -z "$(git status --porcelain)" ] || die "the checkout has uncommitted changes; only a clean HEAD is released"
COMMIT="$(git rev-parse HEAD)"
ORIGIN="$(git remote get-url origin)"
[[ "${ORIGIN}" =~ github\.com[:/]([A-Za-z0-9._-]+)/([A-Za-z0-9._-]+)$ ]] || die "origin ${ORIGIN} is not a GitHub repository"
SLUG="${BASH_REMATCH[1]}/${BASH_REMATCH[2]%.git}"
REPOSITORY="${SLUG#*/}"
DOWNLOAD="${MICA_RELEASE_DOWNLOAD:-https://github.com/${SLUG}/releases/download}"
GIT_URL="${MICA_RELEASE_GIT:-https://github.com/${SLUG}.git}"
# The registry API; the references written into the lock always name ghcr.io.
REGISTRY="${MICA_RELEASE_REGISTRY:-https://ghcr.io}"
OCI_NAME="$(printf '%s' "${SLUG}" | tr '[:upper:]' '[:lower:]')"
OCI_REF="ghcr.io/${OCI_NAME}"
LOCK="${REPOSITORY}.lock"

bash scripts/build/locks.sh check >/dev/null

# The package set: what the producers declare.
ROWS_TEXT="$(bash scripts/deb/producers.sh)" || die "scripts/deb/producers.sh failed"
mapfile -t ROWS <<<"${ROWS_TEXT}"
PACKAGES=()
declare -A WANT=() PRODUCER_OF=()
for row in "${ROWS[@]}"; do
    read -r producer dir packages _enablement <<<"${row}"
    version="$(sed -n 's/^VERSION="\([^"]*\)"$/\1/p' "${dir}/producer.env")"
    for p in $(printf '%s' "${packages}" | tr ',' ' '); do
        PACKAGES+=("${p}")
        WANT["${p}"]="${version}"
        PRODUCER_OF["${p}"]="${producer}"
    done
done
[ "${#PACKAGES[@]}" -gt 0 ] || die "scripts/deb/producers.sh declares no package"

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
mkdir -p "${WORK}/assets" "${WORK}/download"

for arch in "${ARCHES[@]}"; do
    dir="_out/debs/${arch}/pool"
    inputs="_out/debs/${arch}/inputs.tsv"
    [ -f "${inputs}" ] || die "${inputs} does not exist; the pool was not built by scripts/deb/build.sh"
    : >"${WORK}/archives.${arch}.tsv" # file TAB package TAB sha256 TAB size TAB inputs
    mapfile -t debs < <(find "${dir}" -maxdepth 1 -type f -name '*.deb' 2>/dev/null | LC_ALL=C sort)
    [ "${#debs[@]}" -eq "${#PACKAGES[@]}" ] || die "${dir} holds ${#debs[@]} archives; ${#PACKAGES[@]} (${PACKAGES[*]}) are released per architecture"
    for p in "${PACKAGES[@]}"; do
        deb="${dir}/${p}_${WANT[${p}]}_${arch}.deb"
        [ -f "${deb}" ] || die "${deb} does not exist; ${p} is released at its declared version ${WANT[${p}]}"
        field() { dpkg-deb --field "${deb}" "$1"; }
        [ "$(field Package)" = "${p}" ] || die "${deb} is Package $(field Package), not ${p}"
        [ "$(field Version)" = "${WANT[${p}]}" ] || die "${deb} is Version $(field Version), not ${WANT[${p}]}"
        [ "$(field Architecture)" = "${arch}" ] || die "${deb} is Architecture $(field Architecture), not ${arch}"
        [ "$(field Mica-Source-Repo)" = "${REPOSITORY}" ] || die "${deb} carries Mica-Source-Repo $(field Mica-Source-Repo), not ${REPOSITORY}"
        [ -z "$(field Mica-Source-Commit)" ] || die "${deb} carries Mica-Source-Commit; a package carries no commit"
        hash="$(awk -F'\t' -v p="${PRODUCER_OF[${p}]}" '$1 == p { print $2 }' "${inputs}")"
        [[ "${hash}" =~ ^[0-9a-f]{64}$ ]] || die "${inputs} records no inputs hash for ${PRODUCER_OF[${p}]}"
        printf '%s\t%s\t%s\t%s\t%s\n' "${deb}" "${p}" "$(sha256sum "${deb}" | cut -d' ' -f1)" "$(stat -c %s "${deb}")" "${hash}" >>"${WORK}/archives.${arch}.tsv"
    done
done

[ -n "${GH_TOKEN:-}" ] || die "GH_TOKEN must be set; releasing is CI's, with its own token"
bash scripts/build/reuse.sh --exclude "${TAG}" >"${WORK}/reuse.txt" || die "the packages do not pass the version guard against the previous release"
sed 's/^/release.sh: /' "${WORK}/reuse.txt"
git merge-base --is-ancestor "${COMMIT}" origin/main 2>/dev/null || die "${COMMIT} is not on origin/main; only a commit of main is released"

tag_commit() { # the commit the tag names at GIT_URL, read anonymously
    git ls-remote --tags "${GIT_URL}" 2>/dev/null | awk -v t="refs/tags/${TAG}" '$2 == t || $2 == t "^{}" { sha = $1 } END { print sha }'
}
tagged="$(tag_commit)"
[ "${tagged}" = "${COMMIT}" ] || die "tag ${TAG} is ${tagged:-absent} at ${GIT_URL}, not HEAD ${COMMIT}"

if ! gh api "repos/${SLUG}/releases/tags/${TAG}" >"${WORK}/release.json" 2>"${WORK}/release.err"; then
    grep -c 'HTTP 404' "${WORK}/release.err" >/dev/null && die "there is no published release ${TAG} in ${SLUG}; cut it with gh release create ${TAG} --target ${COMMIT}"
    die "reading release ${TAG} of ${SLUG} failed: $(head -c 300 "${WORK}/release.err")"
fi
[ "$(jq -r .tag_name "${WORK}/release.json")" = "${TAG}" ] &&
    [ "$(jq -r .html_url "${WORK}/release.json")" = "https://github.com/${SLUG}/releases/tag/${TAG}" ] ||
    die "$(jq -r .html_url "${WORK}/release.json") is not the release ${TAG} of ${SLUG}"
[ "$(jq -r .draft "${WORK}/release.json")" = false ] || die "release ${TAG} of ${SLUG} is a draft; assets go to a published release only"
while IFS= read -r n; do
    case "${n}" in "${LOCK}" | SHA256SUMS) ;; *) die "release ${TAG} carries ${n}; a release carries only ${LOCK} and SHA256SUMS" ;; esac
done < <(jq -r '.assets[].name' "${WORK}/release.json")

# The OCI pools, through the Distribution API: a bearer token from the
# registry's challenge, with GH_TOKEN for pushing and with no credential for
# reading back.
oci_token() { # <scope> <push|anonymous>
    local challenge realm service
    challenge="$(curl -sS -o /dev/null -D - "${REGISTRY}/v2/" | tr -d '\r' | sed -n 's/^[Ww][Ww][Ww]-[Aa]uthenticate: *//p')"
    [ -n "${challenge}" ] || return 0
    realm="$(printf '%s' "${challenge}" | sed -n 's/.*realm="\([^"]*\)".*/\1/p')"
    service="$(printf '%s' "${challenge}" | sed -n 's/.*service="\([^"]*\)".*/\1/p')"
    [ -n "${realm}" ] || return 1
    if [ "$2" = push ]; then
        curl -fsS -u "${GITHUB_ACTOR:-${REPOSITORY}}:${GH_TOKEN}" -G --data-urlencode "service=${service}" --data-urlencode "scope=$1" "${realm}"
    else
        curl -fsS -G --data-urlencode "service=${service}" --data-urlencode "scope=$1" "${realm}"
    fi | jq -r '.token // .access_token // empty'
}
oci_curl() { # <token> <curl arguments>...
    local token="$1"
    shift
    if [ -n "${token}" ]; then curl -H "Authorization: Bearer ${token}" "$@"; else curl "$@"; fi
}
MANIFEST_TYPE=application/vnd.oci.image.manifest.v1+json

PUSH_TOKEN="$(oci_token "repository:${OCI_NAME}:pull,push" push)" || die "the registry ${REGISTRY} refused a push token for ${OCI_NAME}"
push_blob() { # <file> <digest>
    local code location separator
    code="$(oci_curl "${PUSH_TOKEN}" -sS -o /dev/null -w '%{http_code}' -I "${REGISTRY}/v2/${OCI_NAME}/blobs/$2")"
    [ "${code}" = 200 ] && return 0
    location="$(oci_curl "${PUSH_TOKEN}" -fsS -X POST -o /dev/null -D - "${REGISTRY}/v2/${OCI_NAME}/blobs/uploads/" | tr -d '\r' | sed -n 's/^[Ll]ocation: *//p')"
    [ -n "${location}" ] || die "the registry started no blob upload for ${OCI_NAME}"
    case "${location}" in http://* | https://*) ;; *) location="${REGISTRY}${location}" ;; esac
    case "${location}" in *\?*) separator='&' ;; *) separator='?' ;; esac
    oci_curl "${PUSH_TOKEN}" -fsS -o /dev/null -X PUT -H 'Content-Type: application/octet-stream' \
        --data-binary "@$1" "${location}${separator}digest=$2" || die "uploading $1 to ${OCI_NAME} failed"
}

printf '{}' >"${WORK}/config.json"
CONFIG_DIGEST="sha256:$(sha256sum "${WORK}/config.json" | cut -d' ' -f1)"
push_blob "${WORK}/config.json" "${CONFIG_DIGEST}"
private="GHCR creates a new package private; make ${OCI_REF} public in the package settings, then run this again"
ANON_TOKEN="$(oci_token "repository:${OCI_NAME}:pull" anonymous || true)"
declare -A POOL_DIGEST=()

for arch in "${ARCHES[@]}"; do
    tag="pool.${arch}.${TAG}"
    while IFS=$'\t' read -r file _p sha _size _inputs; do
        push_blob "${file}" "sha256:${sha}"
    done <"${WORK}/archives.${arch}.tsv"
    # Release-independent: an unchanged pool is byte-identical in every release.
    jq -Rn --arg config "${CONFIG_DIGEST}" --arg arch "${arch}" --arg repository "${REPOSITORY}" '{
        schemaVersion: 2,
        mediaType: "application/vnd.oci.image.manifest.v1+json",
        artifactType: "application/vnd.mica.pool",
        config: {mediaType: "application/vnd.oci.empty.v1+json", digest: $config, size: 2},
        layers: [inputs | split("\t") | {
            mediaType: "application/vnd.mica.deb",
            digest: ("sha256:" + .[2]),
            size: (.[3] | tonumber),
            annotations: {"org.opencontainers.image.title": (.[0] | split("/") | last), "mica.inputs": .[4]}
        }],
        annotations: {
            "mica.source-repo": $repository,
            "mica.arch": $arch
        }
    }' <"${WORK}/archives.${arch}.tsv" | jq -c . | tr -d '\n' >"${WORK}/manifest.${arch}.json"
    digest="sha256:$(sha256sum "${WORK}/manifest.${arch}.json" | cut -d' ' -f1)"

    code="$(oci_curl "${PUSH_TOKEN}" -sS -o "${WORK}/existing.json" -w '%{http_code}' -H "Accept: ${MANIFEST_TYPE}" "${REGISTRY}/v2/${OCI_NAME}/manifests/${tag}")"
    case "${code}" in
    200)
        [ "sha256:$(sha256sum "${WORK}/existing.json" | cut -d' ' -f1)" = "${digest}" ] ||
            die "${OCI_REF}:${tag} already holds another manifest; a published tag is never re-pointed"
        echo "release.sh: ${OCI_REF}:${tag} already holds this manifest"
        ;;
    404)
        oci_curl "${PUSH_TOKEN}" -fsS -o /dev/null -X PUT -H "Content-Type: ${MANIFEST_TYPE}" \
            --data-binary "@${WORK}/manifest.${arch}.json" "${REGISTRY}/v2/${OCI_NAME}/manifests/${tag}" ||
            die "pushing the manifest ${OCI_REF}:${tag} failed"
        echo "release.sh: pushed ${OCI_REF}:${tag} (${digest}, $(grep -c . "${WORK}/archives.${arch}.tsv") archives)"
        ;;
    *) die "reading ${OCI_REF}:${tag} answered HTTP ${code}" ;;
    esac

    # Anonymous: the manifest by tag, then every layer by digest.
    [ -n "${ANON_TOKEN}" ] || ANON_TOKEN="$(oci_token "repository:${OCI_NAME}:pull" anonymous)" || die "no anonymous pull token for ${OCI_REF}; ${private}"
    code="$(oci_curl "${ANON_TOKEN}" -sS -o "${WORK}/anonymous.json" -w '%{http_code}' -H "Accept: ${MANIFEST_TYPE}" "${REGISTRY}/v2/${OCI_NAME}/manifests/${tag}")"
    [ "${code}" = 200 ] || die "${OCI_REF}:${tag} cannot be read anonymously (HTTP ${code}); ${private}"
    [ "sha256:$(sha256sum "${WORK}/anonymous.json" | cut -d' ' -f1)" = "${digest}" ] || die "${OCI_REF}:${tag} reads anonymously as another manifest"
    while IFS=$'\t' read -r file _p sha _size _inputs; do
        oci_curl "${ANON_TOKEN}" -fsSL -o "${WORK}/layer" "${REGISTRY}/v2/${OCI_NAME}/blobs/sha256:${sha}" || die "the layer of ${file} cannot be read anonymously; ${private}"
        [ "$(sha256sum "${WORK}/layer" | cut -d' ' -f1)" = "${sha}" ] || die "the layer of ${file} reads anonymously with other bytes"
    done <"${WORK}/archives.${arch}.tsv"
    echo "release.sh: ${OCI_REF}:${tag} read back anonymously (${digest})"
    POOL_DIGEST["${arch}"]="${digest}"
done

# The lock: rows in the order of the specification (release, pool, package), each by key.
{
    echo "# mica-lock v1"
    printf 'release\t%s\t%s\t%s\n' "${REPOSITORY}" "${TAG}" "${COMMIT}"
    for arch in "${ARCHES[@]}"; do printf 'pool\t%s\t%s:pool.%s.%s@%s\n' "${arch}" "${OCI_REF}" "${arch}" "${TAG}" "${POOL_DIGEST[${arch}]}"; done
    for arch in "${ARCHES[@]}"; do
        while IFS=$'\t' read -r _file p sha _size _inputs; do printf 'package\t%s\t%s\t%s\t%s\n' "${p}" "${arch}" "${WANT[${p}]}" "${sha}"; done <"${WORK}/archives.${arch}.tsv"
    done | LC_ALL=C sort -t$'\t' -k2,2 -k3,3
} >"${WORK}/assets/${LOCK}"
result="$(bash scripts/build/check-lock.sh lock "${WORK}/assets/${LOCK}")" || die "the lock this release writes is ${result}"
(cd "${WORK}/assets" && sha256sum -- "${LOCK}" >SHA256SUMS)

# Attached assets: each must be uploaded and hold these bytes; the lock goes before SHA256SUMS.
missing=()
for n in "${LOCK}" SHA256SUMS; do
    f="${WORK}/assets/${n}"
    attached="$(jq -r --arg n "${n}" '.assets[] | select(.name == $n) | "\(.state) \(.digest)"' "${WORK}/release.json")"
    if [ -z "${attached}" ]; then
        missing+=("${f}")
        continue
    fi
    [ "${attached%% *}" = uploaded ] || die "${n} is attached but not uploaded (state ${attached%% *}); delete that asset by hand after inspection, then rerun"
    [ "${attached#* }" = "sha256:$(sha256sum "${f}" | cut -d' ' -f1)" ] ||
        die "${n} is already attached with other bytes (${attached#* }); a published asset is never replaced"
done
if [ "${#missing[@]}" -gt 0 ]; then
    for f in "${missing[@]}"; do gh release upload "${TAG}" "${f}" -R "${SLUG}" >/dev/null; done
    echo "release.sh: attached ${#missing[@]} asset(s) to ${SLUG} ${TAG}"
else
    echo "release.sh: ${SLUG} ${TAG} already carries these assets"
fi

# Anonymous: the tag, then both assets through the download URL.
tagged="$(tag_commit)"
[ "${tagged}" = "${COMMIT}" ] || die "tag ${TAG} is ${tagged:-absent} at ${GIT_URL}, not ${COMMIT}"
for n in "${LOCK}" SHA256SUMS; do
    curl -fsSL --retry 5 --retry-delay 5 --max-time 600 -o "${WORK}/download/${n}" "${DOWNLOAD}/${TAG}/${n}" ||
        die "${DOWNLOAD}/${TAG}/${n} cannot be downloaded anonymously"
    cmp -s "${WORK}/download/${n}" "${WORK}/assets/${n}" || die "${DOWNLOAD}/${TAG}/${n} downloads with other bytes"
done
(cd "${WORK}/download" && sha256sum --quiet -c SHA256SUMS) || die "the downloaded ${LOCK} of ${TAG} does not match SHA256SUMS"
echo "release.sh: ${DOWNLOAD}/${TAG}/ downloaded anonymously, tag ${TAG} at ${COMMIT}"
echo "release.sh: SHA256SUMS sha256 $(sha256sum "${WORK}/assets/SHA256SUMS" | cut -d' ' -f1)"
sed 's/^/release.sh:   /' "${WORK}/assets/${LOCK}"
