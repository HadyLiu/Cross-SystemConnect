# Toolchain only. Application source is mounted when compiling.
#   bash scripts/package-docker.sh
FROM rust:1-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential pkg-config \
        libwebkit2gtk-4.1-dev libgtk-3-dev \
        libx11-dev libxi-dev libxtst-dev libxfixes-dev libxrandr-dev \
        libxdo-dev libayatana-appindicator3-dev \
        gcc-mingw-w64-x86-64 zip \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add x86_64-pc-windows-gnu

WORKDIR /src
CMD ["bash", "packaging/compile.sh"]
