#!/usr/bin/env bash
# A producer's prepare step: compile its binaries and stage them.
#
#   bash scripts/build/build-deb.sh --producer <name> --bins "<binary> ..." \
#        --arch <amd64|arm64> --stage <dir>
#
# Compiles only the named binaries, in the mica-build-env rust image of the
# host's architecture (native, or a cargo cross-compile; never emulation), into
# the producer's own target directory _out/target-deb/<producer>. It then fails
# if that directory holds a binary the producer does not own, checks each
# binary's ELF architecture (and that mica-runkit is static), and copies the
# binaries into --stage. The repository is mounted read-only at /src so rustc's
# recorded paths do not depend on where the checkout lives.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
die() { echo "build-deb.sh: error: $*" >&2; exit 1; }
command -v docker >/dev/null 2>&1 || die "docker is required"

ALL_BINARIES=(micad mica-apid mica-mqttd mica-mqtt-broker mica-mqtt-reference mica-sftp-server mica-deploy mica-runkit)
PRODUCER="" BINS="" ARCH="" STAGE=""
while [ "$#" -gt 0 ]; do
    case "$1" in
    --producer) PRODUCER="${2:?}"; shift 2 ;;
    --bins) BINS="${2:?}"; shift 2 ;;
    --arch) ARCH="${2:?}"; shift 2 ;;
    --stage) STAGE="${2:?}"; shift 2 ;;
    *) die "usage: bash scripts/build/build-deb.sh --producer <name> --bins \"<binary> ...\" --arch <amd64|arm64> --stage <dir>" ;;
    esac
done
[ -n "${PRODUCER}" ] && [ -n "${BINS}" ] && [ -d "${STAGE}" ] || die "--producer, --bins and an existing --stage are required"
case "${ARCH}" in
amd64) TRIPLE=x86_64-unknown-linux-gnu ELF_ARCH=x86-64 ;;
arm64) TRIPLE=aarch64-unknown-linux-gnu ELF_ARCH=aarch64 ;;
*) die "--arch must be amd64 or arm64" ;;
esac
EXCLUDED=()
for b in ${BINS}; do
    case " ${ALL_BINARIES[*]} " in *" ${b} "*) ;; *) die "${b} is not one of ${ALL_BINARIES[*]}" ;; esac
done
for b in "${ALL_BINARIES[@]}"; do
    case " ${BINS} " in *" ${b} "*) ;; *) EXCLUDED+=("${b}") ;; esac
done
case "$(uname -m)" in x86_64) HOST_ARCH=amd64 ;; aarch64 | arm64) HOST_ARCH=arm64 ;; *) die "unsupported host $(uname -m)" ;; esac

COMMIT="$(git -C "${REPO_ROOT}" rev-parse --short=12 HEAD)"
[ -z "$(git -C "${REPO_ROOT}" status --porcelain)" ] || COMMIT="${COMMIT}-dirty"
MICA_BUILD_COMMIT="${MICA_BUILD_COMMIT:-${COMMIT}}"
TARGET_DIR="${REPO_ROOT}/_out/target-deb/${PRODUCER}"
CARGO_CACHE="${REPO_ROOT}/_out/cargo"
mkdir -p "${CARGO_CACHE}/registry" "${CARGO_CACHE}/git" "${TARGET_DIR}"
RUST_IMAGE="$(bash "${REPO_ROOT}/scripts/build/from.sh" --arch="${HOST_ARCH}" --ref rust)"

# mica-apid embeds the built UI.
APID_UI_ARGS=()
case " ${BINS} " in
*" mica-apid "*)
    APID_UI_DIST="${REPO_ROOT}/_out/apid-ui/dist"
    bash "${REPO_ROOT}/crates/mica-apid/ui/build.sh"
    APID_UI_ARGS=(-v "${APID_UI_DIST}:/build/apid-ui:ro" -e "MICA_APID_UI_DIST_DIR=/build/apid-ui")
    ;;
esac

docker run --rm --label ai-agent=true --platform "linux/${HOST_ARCH}" \
    -v "${REPO_ROOT}:/src:ro" \
    -v "${TARGET_DIR}:/target" \
    -v "${CARGO_CACHE}/registry:/usr/local/cargo/registry" \
    -v "${CARGO_CACHE}/git:/usr/local/cargo/git" \
    -w /src \
    -e "TARGET=${TRIPLE}" -e "ELF_ARCH=${ELF_ARCH}" -e "BINS=${BINS}" \
    -e "CARGO_TARGET_DIR=/target" -e "MICA_BUILD_COMMIT=${MICA_BUILD_COMMIT}" \
    "${APID_UI_ARGS[@]}" \
    --entrypoint /bin/bash "${RUST_IMAGE}" -c '
        set -euo pipefail
        . /etc/mica-build/rust.env
        echo "build-deb.sh: compiling ${BINS} for ${TARGET} with rustc ${MICA_BUILD_RUSTC}"
        bins=""
        for b in ${BINS}; do [ "${b}" = mica-runkit ] || bins="${bins} --bin ${b}"; done
        # shellcheck disable=SC2086
        [ -z "${bins}" ] || cargo build --release --locked --target "${TARGET}" ${bins}
        for name in ${BINS}; do
            if [ "${name}" = mica-runkit ]; then
                # Static, in its own target directory so its rustflags reach no other binary.
                env -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS CARGO_TARGET_DIR="${CARGO_TARGET_DIR}/runkit-static" \
                    cargo build --release --locked --target "${TARGET}" --bin "${name}" \
                    --config "target.${TARGET}.rustflags=[\"-C\",\"target-feature=+crt-static\",\"-C\",\"strip=symbols\"]"
                bin="${CARGO_TARGET_DIR}/runkit-static/${TARGET}/release/${name}"
                if readelf -W -l "${bin}" | grep -Ec "^[[:space:]]*INTERP[[:space:]]" >/dev/null ||
                    readelf -W -d "${bin}" | grep -Ec "\(NEEDED\)|\(RPATH\)|\(RUNPATH\)" >/dev/null; then
                    echo "build-deb.sh: error: ${name} is not a standalone static ELF" >&2
                    exit 1
                fi
            else
                bin="${CARGO_TARGET_DIR}/${TARGET}/release/${name}"
            fi
            [ -f "${bin}" ] || { echo "build-deb.sh: error: ${name} was not produced" >&2; exit 1; }
            case "$(file -b "${bin}")" in
            *"ELF 64-bit"*"${ELF_ARCH}"*) ;;
            *) echo "build-deb.sh: error: ${name} is not an ${ELF_ARCH} ELF" >&2; exit 1 ;;
            esac
        done
    '

stray=""
for name in "${EXCLUDED[@]}"; do
    stray="${stray}$(find "${TARGET_DIR}" -type f -name "${name}" -printf ' %p')"
done
[ -z "${stray}" ] || die "${TARGET_DIR} holds binaries the ${PRODUCER} producer does not own:${stray}"
for name in ${BINS}; do
    if [ "${name}" = mica-runkit ]; then
        cp "${TARGET_DIR}/runkit-static/${TRIPLE}/release/${name}" "${STAGE}/${name}"
    else
        cp "${TARGET_DIR}/${TRIPLE}/release/${name}" "${STAGE}/${name}"
    fi
done
echo "build-deb.sh: staged ${BINS} into ${STAGE}"
