#!/usr/bin/env bash
# The Rust gate, run inside the mica-build-env rust image by scripts/gate/rust-gate.sh.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE="$(cd "${HERE}/../.." && pwd)"
cd "${WORKSPACE}"
export PATH="$HOME/.cargo/bin:$PATH"

# EVERY PRODUCER'S VERSION IS ITS BINARIES' CRATE VERSION. pkgs/<producer>/producer.env
# declares VERSION=<upstream>-<revision>; the upstream part must be the version of
# the crate of every binary the producer ships (its prepare.sh --bins), so the
# version a package carries is the version its binaries were built as.
metadata="$(cargo metadata --locked --no-deps --format-version 1)"
for env_file in pkgs/*/producer.env; do
    producer="$(basename "$(dirname "${env_file}")")"
    declared="$(sed -n 's/^VERSION="\([^"]*\)"$/\1/p' "${env_file}")"
    upstream="${declared%-*}"
    bins="$(sed -n 's/.*--bins "\([^"]*\)".*/\1/p' "pkgs/${producer}/prepare.sh")"
    [ -n "${declared}" ] && [ -n "${bins}" ] || {
        echo "error: ${env_file} declares no VERSION or pkgs/${producer}/prepare.sh names no --bins" >&2
        exit 1
    }
    for bin in ${bins}; do
        crate="$(jq -r --arg b "${bin}" '.packages[] | select(any(.targets[]; .name == $b and (.kind | index("bin")))) | "\(.name) \(.version)"' <<<"${metadata}")"
        [ "${crate#* }" = "${upstream}" ] || {
            echo "error: ${env_file} declares ${declared}, but ${bin} is built from ${crate:-no crate}; the upstream part of a producer's version is its binaries' crate version" >&2
            exit 1
        }
    done
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
    echo '         cargo run -p mica-apid --bin mica-apid -- --openapi > crates/mica-apid/openapi.json' >&2
    exit 1
}
echo "crates/mica-apid/openapi.json matches mica-apid --openapi"

echo "ALL CHECKS PASSED"
