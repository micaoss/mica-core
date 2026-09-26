#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
REL_DIST="crates/mica-apid/ui/dist"
DIST_PROBE="${REL_DIST}/index.html"
BUILD_SCRIPT="${ROOT}/crates/mica-apid/ui/build.sh"
CHECK_SCRIPT="${ROOT}/crates/mica-apid/ui/run.sh"
OUTPUT_REL="_out/apid-ui/dist"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

tracked="$(git -C "${ROOT}" ls-files -- "${REL_DIST}")"
[ -z "${tracked}" ] || fail "${REL_DIST}/ is generated output but Git still tracks files below it"

git -C "${ROOT}" check-ignore --no-index -q "${DIST_PROBE}" ||
    fail "${REL_DIST}/ is generated output but is not ignored"

[ -f "${BUILD_SCRIPT}" ] ||
    fail "the built-in UI has no production build entry"

grep -q 'from.sh" --ref base' "${BUILD_SCRIPT}" ||
    fail "the production build does not select the pinned mica-build-base image, whose bun builds the UI"
if grep -q 'IMAGE_BUN_1' "${BUILD_SCRIPT}"; then
    fail "the production build still selects IMAGE_BUN_1, a second bun"
fi
grep -q -- '-v "${HERE}:/source:ro"' "${BUILD_SCRIPT}" ||
    fail "the production build does not mount UI source read-only"
grep -q -- '-v "${BUILD_ROOT}:/build"' "${BUILD_SCRIPT}" ||
    fail "the production build does not isolate writes below _out/apid-ui"
grep -q 'OUTPUT="${BUILD_ROOT}/dist"' "${BUILD_SCRIPT}" ||
    fail "the production build output is not fixed at ${OUTPUT_REL}"
grep -q '\[ ! -L "${path}" \]' "${BUILD_SCRIPT}" ||
    fail "the production build does not refuse symlinked build-root components"
grep -q -- '--user "$(id -u):$(id -g)"' "${BUILD_SCRIPT}" ||
    fail "the production build does not preserve caller ownership on generated files"
if grep -q 'command -v bun' "${BUILD_SCRIPT}"; then
    fail "the production build still selects a host Bun installation"
fi

grep -q 'build.sh" --check' "${CHECK_SCRIPT}" ||
    fail "the frontend quality gate does not reuse the container-only producer"
if grep -Eq 'bun (install|run)|command -v bun' "${CHECK_SCRIPT}"; then
    fail "the frontend quality gate still owns a second Bun execution path"
fi

# The console is a package of its own (mica-apid-ui), not part of apid's
# binary: nothing compiles it in, and nothing hands its output to Cargo.
[ ! -e "${ROOT}/crates/mica-apid/build.rs" ] ||
    fail "crates/mica-apid/build.rs exists again; the console is served from /usr/share/mica-apid/ui, not embedded"
for entry in \
    "scripts/build/check.sh" \
    "scripts/build/build-deb.sh" \
    "scripts/gate/rust-gate.sh"
do
    if grep -q 'MICA_APID_UI_DIST_DIR' "${ROOT}/${entry}"; then
        fail "${entry} still passes a generated UI directory to Cargo"
    fi
done

# The one place the console is built for a package: the micad producer, which
# stages the ignored output for mica-apid-ui and never the source tree's.
PREPARE="pkgs/micad/prepare.sh"
grep -q 'apid/ui/build.sh' "${ROOT}/${PREPARE}" ||
    fail "${PREPARE} does not build the console for mica-apid-ui"
grep -q '_out/apid-ui/dist' "${ROOT}/${PREPARE}" ||
    fail "${PREPARE} does not stage the console from the ignored build output"
if grep -q 'apid/ui/dist' "${ROOT}/${PREPARE}"; then
    fail "${PREPARE} consumes generated assets from the UI source tree"
fi
grep -q 'mica-apid-ui' "${ROOT}/pkgs/micad/producer.env" ||
    fail "the micad producer does not ship mica-apid-ui"

grep -q -- '-v "${REPO_ROOT}:/src:ro"' "${ROOT}/scripts/build/build-deb.sh" ||
    fail "scripts/build/build-deb.sh does not mount repository source read-only"
grep -q 'CARGO_TARGET_DIR=/target' "${ROOT}/scripts/build/build-deb.sh" ||
    fail "scripts/build/build-deb.sh does not move Cargo writes out of the source tree"

if grep -q 'apid/ui/dist' "${ROOT}/.github/workflows/build.yml"; then
    fail "CI still consumes generated UI assets from the source tree"
fi

echo "APID UI BUILD CONTRACT PASSED"
