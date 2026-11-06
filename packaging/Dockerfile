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

# Release binaries, so a check does not compile or download them again.
ARG AUDIT_VERSION=0.22.2
ARG DENY_VERSION=0.20.2
RUN set -eux \
    && mkdir -p /tmp/csc-tools \
    && curl -fsSL "https://github.com/rustsec/rustsec/releases/download/cargo-audit/v${AUDIT_VERSION}/cargo-audit-x86_64-unknown-linux-musl-v${AUDIT_VERSION}.tgz" \
        | tar -xz -C /tmp/csc-tools \
    && curl -fsSL "https://github.com/EmbarkStudios/cargo-deny/releases/download/${DENY_VERSION}/cargo-deny-${DENY_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
        | tar -xz -C /tmp/csc-tools \
    && install -m 0755 "$(find /tmp/csc-tools -type f -name cargo-audit -print -quit)" /usr/local/cargo/bin/cargo-audit \
    && install -m 0755 "$(find /tmp/csc-tools -type f -name cargo-deny -print -quit)" /usr/local/cargo/bin/cargo-deny \
    && rm -rf /tmp/csc-tools \
    && cargo audit --version \
    && cargo deny --version

WORKDIR /src
CMD ["bash", "packaging/compile.sh"]
