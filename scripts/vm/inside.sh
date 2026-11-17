#!/bin/bash
# In the runner: build micad's test binary, make the guest's initramfs and
# boot it under KVM. usage: inside.sh busybox|systemd <test path>
set -euo pipefail
guest=$1 test=$2
work=/work
bin=$(cargo test --locked --no-run -p micad --lib 2>&1 | grep -o '/target/debug/deps/micad-[0-9a-f]*' | awk 'NR == 1')
[ -x "$bin" ] || { echo "no micad test binary" >&2; exit 1; }
rm -rf "$work/root" && mkdir -p "$work/root"
case "$guest" in
busybox)
    mkdir -p "$work/root"/{bin,proc,sys,dev,run,tmp,opt/tools}
    cp /bin/busybox "$work/root/bin/busybox"
    for tool in "$(command -v ip)" "$(command -v sysctl)"; do cp -L "$tool" "$work/root/opt/tools/"; done
    for tool in "$bin" "$(command -v ip)" "$(command -v sysctl)"; do
        ldd "$tool" | grep -o '/[^ ]*' | while read -r lib; do
            mkdir -p "$work/root$(dirname "$lib")" && cp -L "$lib" "$work/root$lib"
        done
    done
    cp /vm/busybox-init "$work/root/init" && chmod +x "$work/root/init"
    init=/init
    ;;
systemd)
    cp -a /guest-root/. "$work/root/"
    init=/lib/systemd/systemd
    ;;
*) echo "unknown guest $guest" >&2; exit 2 ;;
esac
cp "$bin" "$work/root/test" && echo "$test" >"$work/root/test-name"
(cd "$work/root" && find . | cpio -o -H newc --quiet | gzip -1) >"$work/root.cpio.gz"
timeout 600 qemu-system-x86_64 -enable-kvm -m 3072 -smp 2 -nographic -no-reboot -nic none \
    -kernel /vm/bzImage -initrd "$work/root.cpio.gz" \
    -append "console=ttyS0 rdinit=$init panic=-1 quiet systemd.show_status=0" 2>&1 \
    | tr -d '\r' | tee "$work/console.log" \
    | grep -E '^VM|panicked|assert|left:|right:|^test |test result' || :
grep -q '^VM_TEST_EXIT=0$' "$work/console.log"
