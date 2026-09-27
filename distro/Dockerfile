FROM debian:13
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl build-essential pkg-config live-build debootstrap \
    squashfs-tools xorriso isolinux syslinux-common syslinux-utils \
    libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    libx11-dev libxkbcommon-dev libfontconfig1-dev libudev-dev \
    && rm -rf /var/lib/apt/lists/*
RUN curl --fail https://sh.rustup.rs -o /tmp/rustup.sh \
    && sh /tmp/rustup.sh -y --profile minimal --default-toolchain 1.90.0 \
    && rm /tmp/rustup.sh
ENV PATH="/root/.cargo/bin:${PATH}"
WORKDIR /src
CMD ["./build_iso.sh"]
