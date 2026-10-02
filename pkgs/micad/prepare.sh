#!/usr/bin/env bash
set -euo pipefail
# One executable for micad and apid: `mica-apid` is a symlink to `micad`.
bash "${MICA_DEB_REPO_ROOT}/scripts/build/build-deb.sh" \
    --producer "${MICA_DEB_PRODUCER}" --bins "micad" --arch "${MICA_DEB_ARCH}" --stage "${MICA_DEB_STAGE}"
cp "${MICA_DEB_REPO_ROOT}/crates/mica-apid/openapi.json" "${MICA_DEB_STAGE}/openapi.json"
# The console, for the mica-apid-ui package: built in the pinned base image.
bash "${MICA_DEB_REPO_ROOT}/crates/mica-apid/ui/build.sh"
cp -R "${MICA_DEB_REPO_ROOT}/_out/apid-ui/dist" "${MICA_DEB_STAGE}/ui"
