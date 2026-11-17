# The VM runner: the pinned Rust image, which builds micad's test binary,
# plus QEMU, OVMF and what the guests and their disks are made of.
ARG MICA_RUST_IMAGE
FROM ${MICA_RUST_IMAGE}
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
       qemu-system-x86 ovmf busybox-static cpio iproute2 procps \
       fdisk dosfstools mtools e2fsprogs \
    && rm -rf /var/lib/apt/lists/*
