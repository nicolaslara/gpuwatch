#!/bin/sh
# Builds dist/gpuwatch (x86_64 Linux, glibc 2.17 baseline from manylinux2014,
# so it runs on CentOS 7 / Amazon Linux 2 / Ubuntu 18.04 and newer) and the
# wheel dist/modal_gpuwatch-*.whl that carries it. NVML is dlopen'd at
# runtime, which rules out a fully static binary. Needs Docker.
set -eu
cd "$(dirname "$0")"
mkdir -p dist python/gpuwatch/bin
docker run --rm -v "$PWD":/src -w /src -v gpuwatch-cargo:/root/.cargo/registry \
  quay.io/pypa/manylinux2014_x86_64 sh -c '
    set -e
    command -v cargo >/dev/null || curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null
    . "$HOME/.cargo/env"
    cargo build --release --target-dir /src/target/linux
    cp /src/target/linux/release/gpuwatch /src/dist/gpuwatch
    chmod -R a+rwX /src/target/linux /src/dist'
cp dist/gpuwatch python/gpuwatch/bin/gpuwatch && cp README.md python/README.md
cp entrypoint.sh install.sh dist/ && cp python/gpuwatch/__init__.py dist/gpuwatch-init.py && cp python/gpuwatch/_boot.py dist/gpuwatch-boot.py
(cd python && rm -rf dist && (command -v uvx >/dev/null && uvx hatchling build -t wheel || python3 -m pip wheel --no-deps -w dist .) >/dev/null && cp dist/*.whl ../dist/)
ls -l dist/
