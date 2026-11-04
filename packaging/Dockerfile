# Toolchain only. Application source is mounted when compiling.
#   bash scripts/package-docker.sh
# 固定版本；官方 rust 镜像以 --profile minimal 安装，不含 clippy/rustfmt，需显式添加。
# 修改本文件会改变内容哈希，CI 自动构建新标签镜像。
FROM rust:1.99.1-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential pkg-config \
        libwebkit2gtk-4.1-dev libgtk-3-dev \
        libx11-dev libxi-dev libxtst-dev libxfixes-dev libxrandr-dev \
        libxdo-dev libayatana-appindicator3-dev \
        gcc-mingw-w64-x86-64 zip \
    && rm -rf /var/lib/apt/lists/* \
    && rustup component add clippy rustfmt \
    && rustup target add x86_64-pc-windows-gnu \
    && cargo clippy --version && cargo fmt --version

WORKDIR /src
CMD ["bash", "packaging/compile.sh"]
