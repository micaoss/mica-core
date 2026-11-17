#!/usr/bin/env bash
# Run one of micad's VM cases, booted under QEMU/KVM on an x86-64 kernel:
#   failover     uplink::tests::vm::vm_uplink_failover (busybox guest: the
#                uplink engine over two namespaced ISPs)
#   dns-systemd  uplink::tests::vm_systemd::vm_dns_follows_on_systemd
#                (systemd guest: DNS following through networkd and resolved)
# usage: MICA_VM_KERNEL=<bzImage> scripts/vm/run.sh failover|dns-systemd
# The kernel needs veth, network namespaces and a serial console (ttyS0);
# a mica uefi-x64 kernel serves. Needs /dev/kvm. Not part of `make check`.
set -euo pipefail
case "${1:-}" in
failover) guest=busybox test=uplink::tests::vm::vm_uplink_failover ;;
dns-systemd) guest=systemd test=uplink::tests::vm_systemd::vm_dns_follows_on_systemd ;;
*) echo 'usage: MICA_VM_KERNEL=<bzImage> scripts/vm/run.sh failover|dns-systemd' >&2; exit 2 ;;
esac
for tool in docker tar; do command -v "$tool" >/dev/null; done
[ -c /dev/kvm ] || { echo 'needs /dev/kvm' >&2; exit 1; }
[ -f "${MICA_VM_KERNEL:-}" ] || { echo 'MICA_VM_KERNEL must name an x86-64 bzImage' >&2; exit 1; }
REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
HOST_REPO=$REPO
case "$REPO" in /root/*) HOST_REPO="/srv/station/root/${REPO#/root/}";; /work/*) HOST_REPO="/srv/station/work/${REPO#/work/}";; esac
OUT="$REPO/_out/vm/$1"
rm -rf "$OUT/kit" && mkdir -p "$OUT/kit" "$OUT/work" "$OUT/target" "$REPO/_out/cargo/registry" "$REPO/_out/cargo/git"
cp "$MICA_VM_KERNEL" "$OUT/kit/bzImage"
cp "$REPO/scripts/vm/inside.sh" "$REPO/scripts/vm/busybox-init" "$OUT/kit/"
RUST=$("$REPO/bin/mica-tools" from --ref "mica-build-env:rust@amd64")
docker image inspect "$RUST" >/dev/null 2>&1 || docker pull --quiet "$RUST" >/dev/null
RUNNER=ai-agent/mica-vm-runner
docker build -q --label ai-agent=true --build-arg "MICA_RUST_IMAGE=$RUST" \
    -t "$RUNNER" -f "$REPO/scripts/vm/runner.Dockerfile" "$REPO/scripts/vm" >/dev/null
guest_root=()
if [ "$guest" = systemd ]; then
    docker build -q --label ai-agent=true -t ai-agent/mica-vm-systemd-root \
        -f "$REPO/scripts/vm/systemd-root.Dockerfile" "$REPO/scripts/vm" >/dev/null
    rm -rf "$OUT/guest-root" && mkdir -p "$OUT/guest-root"
    container=$(docker create --label ai-agent=true ai-agent/mica-vm-systemd-root)
    docker export "$container" | tar -x -C "$OUT/guest-root"
    docker rm -f "$container" >/dev/null
    guest_root=(-v "$HOST_REPO/_out/vm/$1/guest-root:/guest-root:ro")
fi
docker run --rm --label ai-agent=true --name "ai-agent-mica-vm-$1-$$" --device /dev/kvm \
    -v "$HOST_REPO:/src:ro" -v "$HOST_REPO/_out/vm/$1/target:/target" \
    -v "$HOST_REPO/_out/vm/$1/work:/work" -v "$HOST_REPO/_out/vm/$1/kit:/vm:ro" "${guest_root[@]}" \
    -v "$HOST_REPO/_out/cargo/registry:/usr/local/cargo/registry" \
    -v "$HOST_REPO/_out/cargo/git:/usr/local/cargo/git" \
    -e CARGO_TARGET_DIR=/target -w /src --entrypoint /bin/bash "$RUNNER" /vm/inside.sh "$guest" "$test"
