# The VM runner: the pinned Rust image, which builds micad's test binary,
# plus QEMU and what the guests are made of.
ARG MICA_RUST_IMAGE
FROM ${MICA_RUST_IMAGE}
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
       qemu-system-x86 busybox-static cpio iproute2 procps \
    && rm -rf /var/lib/apt/lists/*
