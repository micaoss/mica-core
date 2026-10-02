#!/bin/sh
# Pack one core component: the staged tree as a squashfs with its dm-verity
# hash tree appended, and the unsigned mica/core/v1 record that names it. Each
# is a pool item (mica-build-tools:docs/spec/release-lock.md 1.2.7), of the
# types core.img and core.json. Runs in the pinned alpine image with
# squashfs-tools, cryptsetup and jq, on the build platform: the image format
# does not depend on the target architecture.
#
#   core-image.sh <tree> <template.json> <arch> <epoch> <out-dir>
#
# The template declares package, version, sourceDateEpoch, features, needs and
# root; <epoch> must be its sourceDateEpoch. Writes
# <out>/<package>_<version>_<arch>.core.img and .core.json.
set -eu

die() { echo "core-image.sh: error: $*" >&2; exit 1; }
[ "$#" -eq 5 ] || die "usage: core-image.sh <tree> <template.json> <arch> <epoch> <out-dir>"
tree="$1" template="$2" arch="$3" epoch="$4" out="$5"

package="$(jq -er '.package' "${template}")" || die "${template} names no package"
version="$(jq -er '.version' "${template}")" || die "${template} names no version"
[ "$(jq -er '.sourceDateEpoch' "${template}")" = "${epoch}" ] || die "${template}: sourceDateEpoch is not ${epoch}"
case "${arch}" in amd64 | arm64) ;; *) die "'${arch}' is not amd64 or arm64" ;; esac

# A component holds /usr and /etc and nothing else: it is composed over the
# root's own two trees by overlay at boot.
for entry in "${tree}"/* "${tree}"/.[!.]*; do
    [ -e "${entry}" ] || [ -L "${entry}" ] || continue
    case "${entry##*/}" in usr | etc) ;; *) die "${package}: /${entry##*/} is outside /usr and /etc" ;; esac
done

mkdir -p "${out}"
image="${out}/${package}_${version}_${arch}.core.img"
record="${out}/${package}_${version}_${arch}.core.json"
rm -f "${image}" "${record}"
# SOURCE_DATE_EPOCH sets every file's time and the filesystem's own.
SOURCE_DATE_EPOCH="${epoch}" mksquashfs "${tree}" "${image}" -noappend -comp zstd -processors 1 \
    -all-root -no-xattrs -no-progress -quiet >/dev/null
data_bytes="$(stat -c %s "${image}")"
[ $((data_bytes % 4096)) -eq 0 ] || die "${package}: the squashfs is not aligned to 4096-byte verity blocks"
# The salt is the image's own content identity, as the root's is.
salt="$(sha256sum "${image}" | cut -d' ' -f1)"
formatted="$(veritysetup format "${image}" "${image}" --no-superblock --format 1 --hash sha256 \
    --data-block-size 4096 --hash-block-size 4096 --data-blocks $((data_bytes / 4096)) \
    --hash-offset "${data_bytes}" --salt "${salt}")"
root_hash="$(printf '%s\n' "${formatted}" | sed -n 's/^Root hash:[[:space:]]*\([0-9a-f]\{64\}\)$/\1/p')"
[ -n "${root_hash}" ] || die "${package}: veritysetup returned no root hash"
veritysetup verify "${image}" "${image}" "${root_hash}" --no-superblock --format 1 --hash sha256 \
    --data-block-size 4096 --hash-block-size 4096 --data-blocks $((data_bytes / 4096)) \
    --hash-offset "${data_bytes}" --salt "${salt}" >/dev/null || die "${package}: the hash tree does not verify"

# The record, canonical: every object's keys sorted, no whitespace.
jq -cS --arg arch "${arch}" --arg rootHash "${root_hash}" --arg salt "${salt}" \
    --argjson bytes "$(stat -c %s "${image}")" --arg sha256 "$(sha256sum "${image}" | cut -d' ' -f1)" \
    --argjson dataBlocks $((data_bytes / 4096)) --argjson hashOffset "${data_bytes}" '
    {
      schema: "mica/core/v1",
      arch: $arch,
      package: .package,
      version: .version,
      features: .features,
      needs: .needs,
      root: .root,
      content: {
        image: { bytes: $bytes, sha256: $sha256 },
        rootHash: $rootHash,
        verity: { version: 1, algorithm: "sha256", dataBlockSize: 4096, hashBlockSize: 4096,
                  dataBlocks: $dataBlocks, hashOffset: $hashOffset, salt: $salt }
      }
    }' "${template}" | tr -d '\n' >"${record}"
echo "${package} ${version} ${arch}: $(stat -c %s "${image}") bytes, root hash ${root_hash}"
