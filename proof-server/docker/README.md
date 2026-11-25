# GPU proof server image

`gpu.Dockerfile` builds the Midnight proof server with CUDA acceleration as a container image that
behaves like upstream's `midnightntwrk/proof-server`: the same command line, environment variables,
port and parameter directory. Give it the GPU with `--gpus all`; without one it proves on the CPU.

**Running it: [USAGE.md](USAGE.md)** (start command, checks, calibration, configuration,
troubleshooting).

## Platforms and GPUs

- **CPU platforms:** `linux/amd64` and `linux/arm64` (NVIDIA's arm64 server platform, SBSA).
- **GPUs:** the CUDA libraries carry machine code (SASS) for every architecture the image's CUDA 12
  toolkit can target, from Maxwell (`sm_50`) to Blackwell (`sm_121`), and PTX of the newest, which
  the driver compiles for GPUs that came after it. `/opt/midnight-gpu/backend/BUILD_INFO` lists them.
- **Driver:** any CUDA 12 capable driver (R525 or newer). The libraries link the CUDA runtime
  statically, so minor-version compatibility applies; a GPU still needs a driver that supports it.
- **Host:** Linux with Docker and the NVIDIA Container Toolkit, or Windows with Docker Desktop on
  WSL 2 ([USAGE.md](USAGE.md#windows)).

## Build

```bash
proof-server/docker/build-gpu-image.sh --backend <checkout of midnight-bls12-381-cuda>
```

It needs Docker with Buildx and a committed ledger tree. The backend repository is private, so the
script gives the build a bare clone of the local checkout (it must contain the commit `Cargo.lock`
pins); the image holds only the compiled libraries. Each platform is built on the build machine,
arm64 by cross-compilation, and loaded as `proof-server-gpu:<version>-amd64` and `-arm64`; the build
machine's own platform is also tagged `proof-server-gpu:<version>`. `--platforms` selects platforms.

To publish, push the per-platform tags and assemble the multi-platform tags in the registry, as
upstream's release does:

```bash
docker tag proof-server-gpu:9.0.2-amd64 <registry>/<name>:9.0.2-amd64   # and -arm64
docker push <registry>/<name>:9.0.2-amd64                               # and -arm64
docker buildx imagetools create -t <registry>/<name>:9.0.2 -t <registry>/<name>:latest \
  <registry>/<name>:9.0.2-amd64 <registry>/<name>:9.0.2-arm64
```

## Provenance

- Labels: `org.opencontainers.image.revision` (this repository's commit), `midnight.gpu.backend.commit`,
  `midnight.gpu.proofs-0.7.commit`, `midnight.gpu.proofs-0.8.commit`.
- `/opt/midnight-gpu/BUILD_MANIFEST`: the platform, the commits `Cargo.lock` pins, the Rust and CUDA
  compilers.
- `/opt/midnight-gpu/backend/BUILD_INFO`: the backend commit (checked against the pin during the
  build, from clean sources), the build options and the GPU architectures.

## Differences from upstream's image

Upstream builds a statically linked musl binary with Nix. This image uses NVIDIA's CUDA base image
(Ubuntu) because the GPU libraries are C++ and CUDA; it is larger, and it relaxes the base image's
CUDA requirement to `cuda>=12.0` (`NVIDIA_REQUIRE_CUDA`) since the libraries need no newer driver.
