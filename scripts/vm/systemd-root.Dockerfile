# The systemd guest: systemd with networkd, resolved, udev and dbus, which
# boots as the initramfs and runs one micad test once the network stack is up.
FROM debian:trixie
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
       systemd systemd-sysv systemd-resolved udev dbus iproute2 \
    && rm -rf /var/lib/apt/lists/*
COPY mica-vm-test.service /etc/systemd/system/mica-vm-test.service
RUN systemctl enable systemd-networkd.service systemd-resolved.service mica-vm-test.service \
    && systemctl mask getty@tty1.service serial-getty@ttyS0.service console-getty.service \
    && : >/etc/machine-id && echo vm >/etc/hostname
