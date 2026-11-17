#!/bin/bash
# In the runner: build mica-deploy and boot the OpenRC guest once per case
# under OVMF, on a GPT disk with the ESP, SYSTEM and DATA, and read the
# outcome off the console. usage: deadline.sh <case>...
#   held       a counted boot nothing confirms: failed and rebooted at confirmS
#   hang       the same boot hung before mica-data-layout (no ESP mounted):
#              rebooted at confirmS without recording, by the kernel once
#              openrc-init, stuck in the boot runlevel, has not begun the
#              shutdown within 15 s
#   slow       the held boot whose orderly shutdown takes 40 s: the init
#              finishes it, no kernel reboot cuts it short
#   confirmed  a counted boot confirmed before confirmS: left alone
#   plain      a boot of an uncounted entry: the deadline exits at once
set -euo pipefail
work=/work
confirm_s=120
id=$(printf 'a%.0s' $(seq 64))
esp_uuid=4c3e0b9e-1f6a-4d8e-9a51-2f0c7d1e8b30
system_uuid=6b3b3f4e-6c1d-4c3e-9b55-0a8f4a8e6a11
data_uuid=0c6a1d2e-3f4b-4a5c-8d6e-7f8091a2b3c4
cargo build --locked -q -p mica-deploy --bin mica-deploy
bin=/target/debug/mica-deploy

# An EFI variable as efivarfs takes it: attributes, then the UTF-16 string
# with its terminator. systemd-boot sets its variables volatile before the
# kernel starts; firmware refuses a volatile one created at runtime, so here
# they are stored (non-volatile, on this boot's copy of the variable store).
efivar() {
    printf '\x07\x00\x00\x00'
    printf '%s' "$1" | iconv -f UTF-8 -t UTF-16LE
    printf '\x00\x00'
}

disk() { # disk <case dir> <entry file name>
    local dir=$1 entry=$2
    truncate -s 160M "$dir/disk.img"
    sfdisk -q "$dir/disk.img" <<EOF
label: gpt
start=2048, size=65536, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, uuid=$esp_uuid
size=65536, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, uuid=$system_uuid
size=131072, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, uuid=$data_uuid
EOF
    rm -f "$dir/esp.img" "$dir/system.img" "$dir/data.img"
    mkfs.vfat -C "$dir/esp.img" $((65536 / 2)) >/dev/null
    printf 'title Mica\nversion 2\nsort-key mica\nefi /EFI/mica/kernels/%s.efi\n' "$(printf 'c%.0s' $(seq 64))" >"$dir/entry"
    mmd -i "$dir/esp.img" ::/loader ::/loader/entries
    mcopy -i "$dir/esp.img" "$dir/entry" "::/loader/entries/$entry"
    mkdir -p "$dir/system-tree" "$dir/data-tree/meta"
    mkfs.ext4 -q -F -d "$dir/system-tree" "$dir/system.img" 32M
    mkfs.ext4 -q -F -d "$dir/data-tree" "$dir/data.img" 64M
    dd if="$dir/esp.img" of="$dir/disk.img" bs=512 seek=2048 conv=notrunc status=none
    dd if="$dir/system.img" of="$dir/disk.img" bs=512 seek=$((2048 + 65536)) conv=notrunc status=none
    dd if="$dir/data.img" of="$dir/disk.img" bs=512 seek=$((2048 + 131072)) conv=notrunc status=none
}

run_case() {
    local case=$1 dir=$work/$1 root=$work/$1/root entry
    rm -rf "$dir" && mkdir -p "$dir"
    cp -a /guest-root "$root"
    # What the export leaves of the container: a console that is a file, an
    # empty hostname, and /.dockerenv, which makes OpenRC take the root for a
    # container's.
    rm -f "$root/dev/console" "$root/.dockerenv" && mknod -m 0600 "$root/dev/console" c 5 1
    echo vm >"$root/etc/hostname"
    mkdir -p "$root/harness/run" "$root/harness/efivars" "$root/usr/lib/mica"
    install -m 0755 /vm/openrc/harness-init "$root/harness-init"
    install -m 0755 "$bin" "$root/usr/bin/mica-deploy"
    install -m 0755 /src/crates/mica-deploy/dist/mica-trial-deadline.openrc "$root/etc/init.d/mica-trial-deadline"
    ln -s /etc/init.d/mica-trial-deadline "$root/etc/runlevels/sysinit/"
    install -m 0755 /vm/openrc/vm-check "$root/etc/init.d/vm-check"
    ln -s /etc/init.d/vm-check "$root/etc/runlevels/default/"
    if [ "$case" = hang ]; then
        install -m 0755 /vm/openrc/vm-hang "$root/etc/init.d/vm-hang"
        ln -s /etc/init.d/vm-hang "$root/etc/runlevels/boot/"
    fi
    if [ "$case" = slow ]; then
        install -m 0755 /vm/openrc/vm-slow "$root/etc/init.d/vm-slow"
        ln -s /etc/init.d/vm-slow "$root/etc/runlevels/default/"
    fi
    echo INIT=openrc >"$root/usr/lib/mica/product.conf"
    printf 'PARTUUID=%s /boot vfat ro,noatime,nosuid,nodev,noexec,umask=0077 0 0\nPARTUUID=%s /mnt/data ext4 noatime 0 2\n' \
        "$esp_uuid" "$data_uuid" >"$root/etc/fstab"
    echo "$case" >"$root/harness/case"
    echo "$confirm_s" >"$root/harness/confirm-s"
    echo "$system_uuid" >"$root/harness/system-uuid"
    echo "$data_uuid" >"$root/harness/data-uuid"
    efivar "mica-$id.conf" >"$root/harness/efivars/LoaderEntrySelected"
    entry="mica-$id.conf"
    if [ "$case" != plain ]; then
        entry="mica-$id+2-1.conf"
        efivar "\\loader\\entries\\$entry" >"$root/harness/efivars/LoaderBootCountPath"
    fi
    cat >"$root/harness/run/boot.json" <<EOF
{"backend":"uefi","deploymentId":"$id","entry":"mica-$id.conf","kernelId":"$(printf 'c%.0s' $(seq 64))","rootfsId":"$(printf 'd%.0s' $(seq 64))","contentVerified":true,"secureBoot":false,"bootVerified":false}
EOF
    cat >"$root/harness/run/boot-policy.json" <<EOF
{"identity":{"board":"uefi-x64","arch":"amd64","kernelBuildId":"$(printf 'd%.0s' $(seq 64))","kernelRelease":"vm","supportId":"$(printf '0%.0s' $(seq 64))"},"board":{"boot":"uefi","kernel":"uki","firmware":{"format":"efi","partition":1,"path":"EFI/BOOT/BOOTX64.EFI"},"partitions":{"boot":1,"system":2,"data":3},"dataFormat":{"bytesPerInode":16384,"features":"project,quota,^orphan_file,^metadata_csum_seed"},"watchdog":{"confirmS":$confirm_s}},"publicKeys":["AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="],"systemPartUuid":"$system_uuid","dataPartUuid":"$data_uuid"}
EOF
    printf '{"coreSetId":"%s","pending":false}\n' "$(printf 'e%.0s' $(seq 64))" >"$root/harness/run/core.json"
    disk "$dir" "$entry"
    (cd "$root" && find . | cpio -o -H newc --quiet | gzip -1) >"$dir/root.cpio.gz"
    cp /usr/share/OVMF/OVMF_VARS_4M.fd "$dir/vars.fd"
    timeout 900 qemu-system-x86_64 -machine q35 -enable-kvm -m 2048 -smp 2 -nographic -no-reboot -nic none \
        -drive if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd \
        -drive if=pflash,format=raw,file="$dir/vars.fd" \
        -drive file="$dir/disk.img",if=virtio,format=raw \
        -kernel /vm/bzImage -initrd "$dir/root.cpio.gz" \
        -append "console=ttyS0 rdinit=/harness-init panic=-1" 2>&1 \
        | tr -d '\r' | sed -u 's/\x1b\[[0-9;?]*[A-Za-z]//g' >"$dir/console.log" || :
    grep -E '^VM|mica-trial-deadline|reboot: |PID1' "$dir/console.log" | sed "s/^/[$case] /"
    check "$case" "$dir/console.log"
}

expect() { grep -q -e "$2" "$1" || { echo "FAIL: missing: $2"; return 1; }; }
refuse() { ! grep -q -e "$2" "$1" || { echo "FAIL: present: $2"; return 1; }; }

# A held trial failed at confirmS and rebooted by the init's orderly shutdown.
failed_by_the_init() {
    local log=$1
    expect "$log" 'VM deadline-alive=yes' &&
        expect "$log" 'mica-trial-deadline: mica-deploy: the trial was not confirmed within 120s; failing the boot' &&
        expect "$log" 'mica-trial-deadline: {"retired":false}' &&
        expect "$log" 'PID1: Received "reboot" from FIFO' &&
        expect "$log" 'VM shutting down, softlevel=shutdown' &&
        expect "$log" 'reboot: Restarting system' && refuse "$log" 'the kernel does'
}

check() {
    local case=$1 log=$2
    case "$case" in
    held) failed_by_the_init "$log" && refuse "$log" 'VM_TEST_END' ;;
    slow)
        failed_by_the_init "$log" && expect "$log" 'VM slow stop begins, softlevel=shutdown' &&
            expect "$log" 'VM slow stop done'
        ;;
    hang)
        expect "$log" 'VM hanging before mica-data-layout' &&
            expect "$log" 'the ESP is not mounted' &&
            expect "$log" 'rebooting without recording, the try is already spent' &&
            expect "$log" 'VM softlevel while hung=boot' &&
            expect "$log" 'openrc-init has not begun the shutdown within 15s, it is inside a runlevel; the kernel does' &&
            expect "$log" 'reboot: Restarting system' && refuse "$log" 'VM deadline-alive' &&
            refuse "$log" 'PID1: Received'
        ;;
    confirmed)
        expect "$log" 'VM deadline-alive=yes' && expect "$log" "VM confirmed mica-$id.conf" &&
            expect "$log" 'mica-trial-deadline: {"confirmed":true,"trial":false}' &&
            expect "$log" 'VM_TEST_END' && expect "$log" 'reboot: Power down' &&
            refuse "$log" 'reboot: Restarting system'
        ;;
    plain)
        expect "$log" 'mica-trial-deadline: {"trial":false}' && expect "$log" 'VM deadline-alive=no' &&
            expect "$log" 'VM_TEST_END' && expect "$log" 'reboot: Power down' &&
            refuse "$log" 'reboot: Restarting system'
        ;;
    esac || return 1
    # Only the hung boot, rebooted without recording, ends with an error.
    if [ "$case" != hang ]; then
        refuse "$log" 'mica-trial-deadline: Error' && refuse "$log" 'mica-deploy exited with status' || return 1
    fi
    echo "VM_CASE $case PASS"
}

status=0
for case in "$@"; do
    run_case "$case" || status=1
done
echo "VM_TEST_EXIT=$status"
exit "$status"
