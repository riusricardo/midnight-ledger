# syntax=docker/dockerfile:1
#
# The GPU proof server image, for linux/amd64 and linux/arm64: midnight-proof-server built with
# `--features gpu`, and the CUDA backend libraries (riusricardo/midnight-bls12-381-cuda) with SASS for
# every GPU architecture the CUDA toolkit supports, plus PTX of the newest for the GPUs after it. It
# runs like upstream's image (flake.nix): the same entrypoint, port, environment and MIDNIGHT_PP; give
# it the GPU with `docker run --gpus all ...`. Without a GPU it proves on the CPU.
#
# Everything compiles in a stage on the build machine's platform: for linux/arm64 it cross-compiles
# (the aarch64 GNU toolchain, CUDA's SBSA cross libraries, Rust's aarch64 target), so no emulation is
# needed. The runtime stage only copies files.
#
# Build it with proof-server/docker/build-gpu-image.sh: the backend repository is private, so the
# script passes its git history (a bare clone holding the commit Cargo.lock pins) as the build
# context `backend-git`, and Cargo fetches the backend from there instead of GitHub.

ARG CUDA_DEVEL_IMAGE=nvidia/cuda:12.9.2-devel-ubuntu24.04@sha256:16656a1ef115bca9e1f820c6349876f1486d2b3c9a0e615773799fe402960dc5
ARG CUDA_BASE_IMAGE=nvidia/cuda:12.9.2-base-ubuntu24.04@sha256:bb0d3cf98bade70e5bf3e15d92355d1418b9628f59b7ed853e83ea66ff20fa49

FROM --platform=$BUILDPLATFORM ${CUDA_DEVEL_IMAGE} AS build
ARG TARGETARCH
ARG RUST_VERSION=1.99.1
# For arm64: the aarch64 GNU toolchain and CUDA's SBSA cross libraries (NVIDIA's cross-linux-sbsa
# repository); CMake looks for an aarch64 CUDA target in targets/aarch64-linux, the SBSA one is sbsa-linux.
# ICICLE's CMake (cmake/setup.cmake) takes `clang`/`clang++` from PATH whenever they exist, over the
# compilers Cargo passes, so the arm64 build puts wrappers first on PATH that target aarch64.
RUN set -eu; \
    pkgs="build-essential clang libclang-dev cmake git pkg-config perl ca-certificates curl"; \
    case "$TARGETARCH" in \
      amd64) ;; \
      arm64) echo 'deb [signed-by=/usr/share/keyrings/cuda-archive-keyring.gpg] https://developer.download.nvidia.com/compute/cuda/repos/ubuntu2404/cross-linux-sbsa/ /' \
               > /etc/apt/sources.list.d/cuda-cross-sbsa.list; \
             pkgs="$pkgs gcc-aarch64-linux-gnu g++-aarch64-linux-gnu libc6-dev-arm64-cross cuda-cross-sbsa-$(echo "$CUDA_VERSION" | cut -d. -f1,2 | tr . -)"; \
             mkdir -p /usr/local/cuda/targets && ln -s sbsa-linux /usr/local/cuda/targets/aarch64-linux; \
             mkdir -p /opt/aarch64-clang; \
             printf '#!/bin/sh\nexec /usr/bin/clang --target=aarch64-linux-gnu "$@"\n' > /opt/aarch64-clang/clang; \
             printf '#!/bin/sh\nexec /usr/bin/clang++ --target=aarch64-linux-gnu "$@"\n' > /opt/aarch64-clang/clang++; \
             chmod +x /opt/aarch64-clang/clang /opt/aarch64-clang/clang++ ;; \
      *) echo "unsupported TARGETARCH $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    apt-get update; \
    apt-get install -y --no-install-recommends $pkgs; \
    rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --no-modify-path --profile minimal --default-toolchain ${RUST_VERSION}

# The target's Rust triple and cross-compilation settings, sourced by the build steps below.
RUN set -eu; { \
      echo 'case "$TARGETARCH" in'; \
      echo '  amd64) RUST_TARGET=x86_64-unknown-linux-gnu ;;'; \
      echo '  arm64) RUST_TARGET=aarch64-unknown-linux-gnu'; \
      echo '         export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc'; \
      echo '         export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++'; \
      echo '         export AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar PKG_CONFIG_ALLOW_CROSS=1'; \
      echo '         export PATH=/opt/aarch64-clang:$PATH'; \
      echo '         export BINDGEN_EXTRA_CLANG_ARGS_aarch64_unknown_linux_gnu=--sysroot=/usr/aarch64-linux-gnu ;;'; \
      echo 'esac'; \
    } > /opt/target-env.sh; \
    . /opt/target-env.sh; rustup target add "$RUST_TARGET"; \
    printf '%s\n' 'set(CMAKE_SYSTEM_NAME Linux)' 'set(CMAKE_SYSTEM_PROCESSOR aarch64)' \
      'set(CMAKE_C_COMPILER aarch64-linux-gnu-gcc)' 'set(CMAKE_CXX_COMPILER aarch64-linux-gnu-g++)' \
      'set(CMAKE_CUDA_HOST_COMPILER aarch64-linux-gnu-g++)' > /opt/aarch64.cmake

# Cargo reaches the private backend through git, rewritten to the bare clone in the build context.
COPY --from=backend-git / /backend.git
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
RUN git config --global url.file:///backend.git.insteadOf https://github.com/riusricardo/midnight-bls12-381-cuda

WORKDIR /src
COPY . .
RUN cargo fetch --locked

RUN set -eu; . /opt/target-env.sh; \
    cargo build --release --locked --target "$RUST_TARGET" -p midnight-proof-server --features gpu; \
    mkdir -p /out/bin /out/lib; \
    cp "target/$RUST_TARGET/release/midnight-proof-server" /out/bin/; \
    cp $(find "target/$RUST_TARGET/release" -path '*/deps/icicle/lib/*.so') /out/lib/

# The CUDA libraries, from Cargo's checkout of exactly the backend commit Cargo.lock pins, for every
# architecture the toolkit lists (`build.sh --arch all`); BUILD_INFO is checked against the pin.
RUN set -eu; \
    rev=$(grep -A2 '^name = "midnight-bls12-381-cuda"' Cargo.lock | grep -oE '#[0-9a-f]{40}' | tr -d '#'); \
    src=$(ls -d "$CARGO_HOME"/git/checkouts/midnight-bls12-381-cuda-*/"$(echo "$rev" | cut -c1-7)"); \
    test "$(git -C "$src" rev-parse HEAD)" = "$rev"; \
    export CUDAFLAGS=-Wno-deprecated-gpu-targets; \
    if [ "$TARGETARCH" = arm64 ]; then export CMAKE_TOOLCHAIN_FILE=/opt/aarch64.cmake CUDAFLAGS="$CUDAFLAGS -target-dir sbsa-linux"; fi; \
    "$src/bls12-381/build.sh" --arch all --install-dir /out/backend; \
    grep -qx "commit=$rev" /out/backend/BUILD_INFO; \
    grep -qx 'library_sources_clean=yes' /out/backend/BUILD_INFO

# What the image was built from, beside the backend's own BUILD_INFO; and the runtime's writable dirs.
ARG TARGETPLATFORM
ARG LEDGER_COMMIT=unknown
RUN set -eu; \
    { echo "platform=${TARGETPLATFORM}"; \
      echo "ledger_commit=${LEDGER_COMMIT}"; \
      grep -B1 -A2 -E '^name = "(midnight-bls12-381-cuda|midnight-proofs|midnight-curves|icicle-runtime)"' Cargo.lock \
        | grep -E '^(name|version|source)' | paste -d' ' - - - | sed 's/^/locked: /'; \
      echo "rustc=$(rustc --version)"; \
      echo "nvcc=$(nvcc --version | tail -1)"; \
    } > /out/BUILD_MANIFEST; \
    mkdir -p /out/var/cache/midnight/zk-params /out/var/cache/midnight-gpu

FROM ${CUDA_BASE_IMAGE}
COPY --from=build /out/bin/midnight-proof-server /usr/local/bin/midnight-proof-server
COPY --from=build /out/lib/ /opt/midnight-gpu/lib/
COPY --from=build /out/backend/ /opt/midnight-gpu/backend/
COPY --from=build /out/BUILD_MANIFEST /opt/midnight-gpu/BUILD_MANIFEST
COPY --from=build /out/var/ /var/
COPY proof-server/docker/gpu-entrypoint.sh /usr/local/bin/proof-server-entrypoint
# The backend libraries link the CUDA runtime statically and carry SASS for every architecture they
# target, so a CUDA 12 driver of any minor version can run them (minor version compatibility): the
# requirement is relaxed from the base image's own `cuda>=12.9` to `cuda>=12.0`.
# MIDNIGHT_AUTOTUNE_CACHE keeps the GPU calibration (measured at the first start) where a volume
# can hold it across containers.
ENV PORT=6300 \
    MIDNIGHT_PP=/var/cache/midnight/zk-params \
    ICICLE_BACKEND_INSTALL_DIR=/opt/midnight-gpu/backend \
    MIDNIGHT_AUTOTUNE_CACHE=/var/cache/midnight-gpu/autotune-v1.conf \
    LD_LIBRARY_PATH=/opt/midnight-gpu/lib${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}} \
    NVIDIA_DRIVER_CAPABILITIES=compute,utility \
    NVIDIA_REQUIRE_CUDA="cuda>=12.0"
EXPOSE 6300
ARG VERSION=unknown
ARG LEDGER_COMMIT=unknown
ARG BACKEND_COMMIT=unknown
ARG PROOFS_V1_COMMIT=unknown
ARG PROOFS_V2_COMMIT=unknown
LABEL org.opencontainers.image.title="midnight-proof-server-gpu" \
      org.opencontainers.image.description="Midnight proof server with CUDA acceleration (run with --gpus all)" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${LEDGER_COMMIT}" \
      org.opencontainers.image.source="https://github.com/riusricardo/midnight-ledger" \
      midnight.gpu.backend.commit="${BACKEND_COMMIT}" \
      midnight.gpu.proofs-0.7.commit="${PROOFS_V1_COMMIT}" \
      midnight.gpu.proofs-0.8.commit="${PROOFS_V2_COMMIT}"
ENTRYPOINT ["/usr/local/bin/proof-server-entrypoint"]
