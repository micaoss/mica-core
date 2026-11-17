# The OpenRC guest: Debian's openrc, openrc-init as PID 1, and Base's own
# services in Base's order, taken from mica-system-base at the release tag
# MICA_VM_BASE_TAG: mica-kernfs and mica-mdev in sysinit, mica-data-layout
# (fsck, `mount -a` of /etc/fstab, the DATA layout) in boot. The ESP is in
# /etc/fstab only, so nothing mounts it before the boot runlevel. The case
# adds mica-deploy, its sysinit script and the runkit's /run/mica files.
FROM debian:trixie
ARG MICA_VM_BASE_TAG
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
       openrc busybox-static util-linux mount e2fsprogs dosfstools procps \
       ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
RUN set -eu; \
    curl -fsSL "https://github.com/micaoss/mica-system-base/archive/refs/tags/${MICA_VM_BASE_TAG}.tar.gz" \
        | tar -xz -C /tmp; \
    base="/tmp/mica-system-base-${MICA_VM_BASE_TAG}"; \
    install -m 0755 "$base/debs/mica-openrc/payload/etc/init.d/mica-kernfs" \
        "$base/debs/mica-openrc/payload/etc/init.d/mica-data-layout" \
        "$base/debs/mica-mdev/payload/etc/init.d/mica-mdev" /etc/init.d/; \
    install -m 0644 "$base/debs/mica-mdev/payload/etc/mdev.conf" /etc/mdev.conf; \
    install -D -m 0755 "$base/payload/usr/lib/mica/mica-data-layout" /usr/lib/mica/mica-data-layout; \
    rm -rf "$base"; \
    find /etc/runlevels -mindepth 2 -delete; \
    for service in mica-kernfs mica-mdev; do ln -s "/etc/init.d/$service" /etc/runlevels/sysinit/; done; \
    ln -s /etc/init.d/mica-data-layout /etc/runlevels/boot/; \
    mkdir -p /mnt/system /mnt/data /boot
