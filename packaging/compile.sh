#!/usr/bin/env bash
# Compile Linux and Windows release binaries. Run inside the toolchain image.
set -euo pipefail
cd /src

export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
export CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc
export AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar
export WINDRES=x86_64-w64-mingw32-windres

cargo build --release --locked -p csc-core -p csc-ui
cargo build --release --locked --target x86_64-pc-windows-gnu -p csc-core -p csc-ui

mkdir -p dist/linux dist/windows
cp -f target/release/Cross-SystemConnect \
      target/release/Cross-SystemConnect-core \
      resources/app.png \
      dist/linux/
cp -f target/x86_64-pc-windows-gnu/release/Cross-SystemConnect.exe \
      target/x86_64-pc-windows-gnu/release/Cross-SystemConnect-core.exe \
      resources/app.ico \
      dist/windows/
rm -f dist/Cross-SystemConnect-linux-x86_64.tar.gz \
      dist/Cross-SystemConnect-windows-x86_64.zip
tar -C dist/linux -czf dist/Cross-SystemConnect-linux-x86_64.tar.gz .
(
  cd dist/windows
  zip -r ../Cross-SystemConnect-windows-x86_64.zip .
)

if [[ -n "${HOST_UID:-}" && -n "${HOST_GID:-}" ]]; then
  chown -R "${HOST_UID}:${HOST_GID}" dist
fi

echo "Linux:   dist/linux/ and dist/Cross-SystemConnect-linux-x86_64.tar.gz"
echo "Windows: dist/windows/ and dist/Cross-SystemConnect-windows-x86_64.zip"
