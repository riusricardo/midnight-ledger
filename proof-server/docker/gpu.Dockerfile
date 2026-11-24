# syntax=docker/dockerfile:1
#
# The GPU proof server image: midnight-proof-server built with `--features gpu`, and the CUDA
# backend libraries (riusricardo/midnight-bls12-381-cuda) for sm_75 through sm_120, each as SASS
# and PTX. It runs like upstream's image (flake.nix): the same entrypoint, port, environment and
# MIDNIGHT_PP; give it the GPU with `docker run --gpus all ...`. Without a GPU it proves on the CPU.
#
# Build it with proof-server/docker/build-gpu-image.sh: the backend repository is private, so the
# script passes its git history (a bare clone holding the commit Cargo.lock pins) as the build
# context `backend-git`, and Cargo fetches the backend from there instead of GitHub.

ARG CUDA_DEVEL_IMAGE=nvidia/cuda:12.9.2-devel-ubuntu24.04@sha256:16656a1ef115bca9e1f820c6349876f1486d2b3c9a0e615773799fe402960dc5
ARG CUDA_BASE_IMAGE=nvidia/cuda:12.9.2-base-ubuntu24.04@sha256:bb0d3cf98bade70e5bf3e15d92355d1418b9628f59b7ed853e83ea66ff20fa49

FROM ${CUDA_DEVEL_IMAGE} AS build
ARG RUST_VERSION=1.99.1
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential clang libclang-dev cmake git pkg-config perl ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --no-modify-path --profile minimal --default-toolchain ${RUST_VERSION}

# Cargo reaches the private backend through git, rewritten to the bare clone in the build context.
COPY --from=backend-git / /backend.git
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true \
    GIT_CONFIG_COUNT=1 \
    GIT_CONFIG_KEY_0=url.file:///backend.git.insteadOf \
    GIT_CONFIG_VALUE_0=https://github.com/riusricardo/midnight-bls12-381-cuda

WORKDIR /src
COPY . .
RUN cargo fetch --locked

# The CUDA libraries, from Cargo's checkout of exactly the backend commit Cargo.lock pins; the
# default build covers sm_75..sm_120 (SASS + PTX) and writes BUILD_INFO, checked here.
RUN set -eu; \
    rev=$(grep -A2 '^name = "midnight-bls12-381-cuda"' Cargo.lock | grep -oE '#[0-9a-f]{40}' | tr -d '#'); \
    src=$(ls -d "$CARGO_HOME"/git/checkouts/midnight-bls12-381-cuda-*/"$(echo "$rev" | cut -c1-7)"); \
    test "$(git -C "$src" rev-parse HEAD)" = "$rev"; \
    "$src/bls12-381/build.sh" --install-dir /out/backend; \
    grep -qx "commit=$rev" /out/backend/BUILD_INFO; \
    grep -qx 'library_sources_clean=yes' /out/backend/BUILD_INFO

RUN cargo build --release --locked -p midnight-proof-server --features gpu \
    && mkdir -p /out/bin /out/lib \
    && cp target/release/midnight-proof-server /out/bin/ \
    && cp target/release/deps/icicle/lib/*.so /out/lib/

# What the image was built from, beside the backend's own BUILD_INFO.
ARG LEDGER_COMMIT=unknown
RUN { echo "ledger_commit=${LEDGER_COMMIT}"; \
      grep -B1 -A2 -E '^name = "(midnight-bls12-381-cuda|midnight-proofs|midnight-curves|icicle-runtime)"' Cargo.lock \
        | grep -E '^(name|version|source)' | paste -d' ' - - - | sed 's/^/locked: /'; \
      echo "rustc=$(rustc --version)"; \
      echo "nvcc=$(nvcc --version | tail -1)"; \
    } > /out/BUILD_MANIFEST

FROM ${CUDA_BASE_IMAGE}
RUN apt-get update \
    && apt-get install -y --no-install-recommends libstdc++6 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /out/bin/midnight-proof-server /usr/local/bin/midnight-proof-server
COPY --from=build /out/lib/ /opt/midnight-gpu/lib/
COPY --from=build /out/backend/ /opt/midnight-gpu/backend/
COPY --from=build /out/BUILD_MANIFEST /opt/midnight-gpu/BUILD_MANIFEST
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
RUN mkdir -p /var/cache/midnight/zk-params /var/cache/midnight-gpu
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
