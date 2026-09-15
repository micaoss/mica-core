#!/usr/bin/env bash
# The Rust gate, run inside the mica-build-env rust image by scripts/gate/rust-gate.sh.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE="$(cd "${HERE}/../.." && pwd)"
REPO_ROOT="${WORKSPACE}"
cd "${WORKSPACE}"
export PATH="$HOME/.cargo/bin:$PATH"

if [ -z "${MICA_APID_UI_DIST_DIR:-}" ]; then
    bash crates/mica-apid/ui/build.sh
    export MICA_APID_UI_DIST_DIR="${REPO_ROOT}/_out/apid-ui/dist"
fi

# THE REPOSITORY VERSION AND THE CRATE VERSION AGREE. scripts/deb/version.sh
# stamps every archive from ${REPO_ROOT}/VERSION; the binaries report the crate
# version from their manifests. Two numbers, one release: a drift between them
# is a package whose version is not the version the binary inside it prints.
declared="$(tr -d '[:space:]' <"${REPO_ROOT}/VERSION")"
for m in crates/*/Cargo.toml; do
    name="$(sed -n '/^\[package\]/,/^\[/ s/^name[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "${m}" | head -n1)"
    [ -n "${name}" ] || continue
    v="$(sed -n '/^\[package\]/,/^\[/ s/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "${m}" | head -n1)"
    [ "${v}" = "${declared}" ] || {
        echo "error: ${m} declares ${name} at version '${v}' and ${REPO_ROOT}/VERSION declares '${declared}'. The package pool is stamped from VERSION and the binaries report the crate version; move whichever is behind" >&2
        exit 1
    }
done

cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked

# `cargo nextest` does not execute doctests, so this is not a duplicate of the
# line above: without it, a broken doctest passes the gate silently.
cargo test --doc --workspace --locked
cargo deny check licenses bans advisories

# THE `--openapi` FLAG PATH. `cargo nextest run` above already runs the in-tree
# test that compares the committed document against the generated one, so this
# catches no drift that test misses. What it adds is the argv handling in
# `main`, above daemon initialisation, which that test bypasses by calling the
# generator function directly: a flag that stopped printing the document, or
# started reaching for the bus before it did, leaves the test green and the
# documented regeneration command broken.
openapi_tmp="$(mktemp -d)"
trap 'rm -rf "${openapi_tmp}"' EXIT
cargo run --locked -p mica-apid --bin mica-apid -- --openapi >"${openapi_tmp}/openapi.json"
diff -u crates/mica-apid/openapi.json "${openapi_tmp}/openapi.json" || {
    echo "error: crates/mica-apid/openapi.json is not what mica-apid --openapi prints." >&2
    echo "       Regenerate it from :" >&2
    echo "         bash crates/mica-apid/ui/build.sh" >&2
    echo '         MICA_APID_UI_DIST_DIR="$PWD/_out/apid-ui/dist" cargo run -p mica-apid --bin mica-apid -- --openapi > crates/mica-apid/openapi.json' >&2
    exit 1
}
echo "crates/mica-apid/openapi.json matches mica-apid --openapi"

echo "ALL CHECKS PASSED"
