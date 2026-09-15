#!/usr/bin/env bash
set -euo pipefail
bash "${MICA_DEB_REPO_ROOT}/scripts/build/build-deb.sh" \
    --producer "${MICA_DEB_PRODUCER}" --bins "mica-apid" --arch "${MICA_DEB_ARCH}" --stage "${MICA_DEB_STAGE}"
cp "${MICA_DEB_REPO_ROOT}/crates/mica-apid/openapi.json" "${MICA_DEB_STAGE}/openapi.json"
