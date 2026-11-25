# Running the GPU proof server

`ricardomnf/midnight-proof-server-gpu` is the Midnight proof server with CUDA acceleration. It is a
drop-in replacement for upstream's `midnightntwrk/proof-server` of the same version: the same HTTP
API, command line, environment variables, port and parameter directory. With a GPU it proves on
the GPU; without one it proves on the CPU like upstream's image.

## Requirements

- A Linux host, x86-64 (`linux/amd64`) or 64-bit ARM server (`linux/arm64`), or Windows with Docker
  Desktop on WSL 2 ([Windows](#windows)). Docker pulls the right platform by itself.
- An NVIDIA GPU, Maxwell or newer, with a driver that supports CUDA 12 (R525 or newer). A GPU newer
  than the image also needs a driver that supports it.
- On Linux, Docker with the [NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html).
  Check it with:

  ```bash
  docker info | grep -i nvidia     # the "nvidia" runtime is listed
  docker run --rm --gpus all --entrypoint nvidia-smi ricardomnf/midnight-proof-server-gpu:9.0.2
  ```

## Start

```bash
docker run -d --name midnight-proof-server --restart unless-stopped \
  --gpus all -p 6300:6300 \
  -v proof-server-params:/var/cache/midnight/zk-params \
  -v proof-server-gpu:/var/cache/midnight-gpu \
  ricardomnf/midnight-proof-server-gpu:9.0.2
```

| Part | Why |
|---|---|
| `--gpus all` | gives the container the GPU; without it the server proves on the CPU |
| `--restart unless-stopped` | starts the server again after a crash or a reboot |
| `-p 6300:6300` | the API port; `-p 8080:6300` publishes it on host port 8080 instead |
| `proof-server-params` volume | keeps the downloaded public parameters and proving keys across containers |
| `proof-server-gpu` volume | keeps the GPU calibration, measured once on the first start |

Options go after the image name, as with upstream's image, for example
`... ricardomnf/midnight-proof-server-gpu:9.0.2 --num-workers 4 -v`. A command that is not an option
runs as given: `... ricardomnf/midnight-proof-server-gpu:9.0.2 midnight-proof-server --help`.

The first start with an empty parameters volume downloads the parameters and the built-in circuits'
proving keys before the server reports ready; later starts only verify them. Then it initializes the
GPU and, on the first start, measures the GPU calibration once.

The same with Docker Compose:

```yaml
services:
  proof-server:
    image: ricardomnf/midnight-proof-server-gpu:9.0.2
    container_name: midnight-proof-server
    restart: unless-stopped
    ports:
      - "6300:6300"
    volumes:
      - proof-server-params:/var/cache/midnight/zk-params
      - proof-server-gpu:/var/cache/midnight-gpu
    deploy:
      resources:
        reservations:
          devices:
            - driver: nvidia
              count: all
              capabilities: [gpu]
volumes:
  proof-server-params:
  proof-server-gpu:
```

## Windows

The image is a Linux image. On Windows it runs in WSL 2, the engine Docker Desktop uses by default,
and NVIDIA's Windows driver gives WSL 2 the GPU. Windows has not been tested with this image yet.

1. Windows 10 or 11, with WSL up to date (`wsl --update` in PowerShell).
2. The current NVIDIA driver for Windows. It serves CUDA inside WSL 2: do not install an NVIDIA
   driver inside WSL.
3. [Docker Desktop](https://docs.docker.com/desktop/features/gpu/) with the WSL 2 based engine; its
   GPU support needs no NVIDIA Container Toolkit.
4. The same command, on one line for PowerShell:

   ```powershell
   docker run -d --name midnight-proof-server --restart unless-stopped --gpus all -p 6300:6300 -v proof-server-params:/var/cache/midnight/zk-params -v proof-server-gpu:/var/cache/midnight-gpu ricardomnf/midnight-proof-server-gpu:9.0.2
   ```

The container sees the CPUs and memory WSL 2 gives its virtual machine (set in
`%UserProfile%\.wslconfig`), so `--num-workers auto` counts those.

On macOS, Docker has no access to NVIDIA GPUs: the image runs there on the CPU (the `linux/arm64`
image on Apple silicon).

## Check that it runs on the GPU

```bash
curl -s localhost:6300/health     # {"status":"ok",...}: the server answers
curl -s localhost:6300/ready      # {"status":"ok","jobsProcessing":..,"jobsPending":..,"jobCapacity":..}
                                  # HTTP 503 with "busy" while the job queue is full
curl -s localhost:6300/version
docker logs midnight-proof-server 2>&1 | grep -E 'GPU|calibration|Proof workers'
```

A start on the GPU logs, for both proof lines (`midnight-proofs 0.7.x` and `0.8.x`):

```
GPU proving backend for midnight-proofs 0.7.x (IR V0/V1) initialized (warm-up ...)
GPU calibration for midnight-proofs 0.7.x (IR V0/V1): calibrated: MSM crossover K.., NTT crossover K..
...
Proof workers: N (auto: one per available CPU, N available)
```

While it proves, `nvidia-smi` on the host lists the server's process. A GPU operation that fails
runs on the CPU instead and logs a `GPU-FALLBACK` line:

```bash
docker logs midnight-proof-server 2>&1 | grep -c GPU-FALLBACK     # 0 when all is well
```

## Day to day

```bash
docker logs -f midnight-proof-server        # follow the log
docker restart midnight-proof-server
docker stop midnight-proof-server
docker start midnight-proof-server
docker update --restart unless-stopped midnight-proof-server   # add the restart policy later
```

## GPU calibration

The server routes each operation to the GPU or the CPU by its size. The sizes where the GPU starts
to pay off (the crossovers) and the MSM window sizes depend on the machine, so the server measures
them on its first start and stores them in `/var/cache/midnight-gpu` (`MIDNIGHT_AUTOTUNE_CACHE`).
Later starts read the file. The file is keyed by the GPU's memory size and the CPU count, and an
image whose calibration format changed measures again by itself.

```bash
# the values in effect (exit code 0: a calibration is in effect; 2: none, e.g. no GPU visible)
docker exec midnight-proof-server midnight-proof-server autotune --show

# measure again, e.g. after changing the GPU or the driver: with the server stopped, so that
# nothing else uses the GPU or the CPUs while it measures
docker stop midnight-proof-server
docker run --rm --gpus all -v proof-server-gpu:/var/cache/midnight-gpu \
  ricardomnf/midnight-proof-server-gpu:9.0.2 midnight-proof-server autotune --refresh
docker start midnight-proof-server
```

`-e MIDNIGHT_AUTOTUNE=refresh` measures again at every start; `-e MIDNIGHT_AUTOTUNE=0` uses
built-in values and never measures.

## Configuration

Each option can also be given as its environment variable (`-e NAME=value`).

| Option | Variable | Default | Meaning |
|---|---|---|---|
| `--num-workers` | `MIDNIGHT_PROOF_SERVER_NUM_WORKERS` | `auto` | proofs proved at once; `auto` is one per CPU available to the container (at least 2) |
| `--job-capacity` | `MIDNIGHT_PROOF_SERVER_JOB_CAPACITY` | 10 | requests that may wait for a worker; more are refused with HTTP 429 |
| `--job-timeout` | `MIDNIGHT_PROOF_SERVER_JOB_TIMEOUT` | 600 | seconds a job may take |
| `--params-cache-mib` | `MIDNIGHT_PROOF_SERVER_PARAMS_CACHE_MIB` | 256 | prover parameters kept in memory between proofs (0: read them for every proof) |
| `--no-fetch-params` | `MIDNIGHT_PROOF_SERVER_NO_FETCH_PARAMS` | off | never download parameters (see below) |
| `-v`, `--verbose` | `MIDNIGHT_PROOF_SERVER_VERBOSE` | off | debug logging, including lines for every GPU transform: for troubleshooting |
| `-p`, `--port` | `MIDNIGHT_PROOF_SERVER_PORT`, else `PORT` | 6300 | the port inside the container |
| | `MIDNIGHT_DEVICE` | `auto` | `auto`: each operation on the GPU or the CPU by size; `gpu` or `cpu` forces one |
| | `MIDNIGHT_GPU_DEVICE` | 0 | which of the GPUs the container sees |
| | `MIDNIGHT_GPU_STRICT` | off | `1`: a GPU failure fails the proof, and start-up fails without a usable GPU |
| | `MIDNIGHT_AUTOTUNE` | on | `0`: built-in calibration; `refresh`: measure at every start |
| | `MIDNIGHT_PARAM_SOURCE` | `https://srs.midnight.network/` | where parameters are downloaded from |

Size `--num-workers` to the proofs your clients send at once, within the host memory their largest
proofs need together; workers beyond that have nothing to do. Clients send a transaction's proofs
together, so bursts above `--job-capacity` are refused: raise it for bursty load.

## Choosing the GPU

```bash
--gpus device=1             # give the container only the second GPU (it appears as GPU 0 inside)
-e MIDNIGHT_GPU_DEVICE=1    # or give it all GPUs and pick one
-e MIDNIGHT_DEVICE=cpu      # prove on the CPU only (or leave out --gpus)
```

One server uses one GPU. For several GPUs, run one container per GPU, each with its own
`--gpus device=N`, port and calibration volume.

## Parameters and keys

The parameters directory is `/var/cache/midnight/zk-params` (`MIDNIGHT_PP`). At start-up the server
makes sure the parameters for k = 10 to 15 and the proving keys of the built-in circuits (Zswap
spend, output and sign, Dust spend) are there, downloads what is missing and checks every file
against its pinned hash. Contract circuits of a larger size need larger parameters; fetch them ahead
of the first proof that needs them:

```bash
curl -s localhost:6300/fetch-params/16      # k from 0 to 25
```

Offline hosts: fill the directory elsewhere (for example from `~/.cache/midnight/zk-params` of a
machine that ran a proof server), mount it read-only and turn fetching off; every file must then
already be there.

```bash
docker run -d ... -v /srv/zk-params:/var/cache/midnight/zk-params:ro \
  ricardomnf/midnight-proof-server-gpu:9.0.2 --no-fetch-params
```

## Upgrading

```bash
docker pull ricardomnf/midnight-proof-server-gpu:<version>
docker rm -f midnight-proof-server
docker run -d --name midnight-proof-server ... ricardomnf/midnight-proof-server-gpu:<version>
```

Use the same `docker run` options as before. The volumes keep the parameters and the calibration.
Tags: `<version>` (for example `9.0.2`) for both platforms, `<version>-amd64` / `<version>-arm64` for
one, and `latest` for the newest release; pin a version in production.

## Troubleshooting

| Symptom | Check |
|---|---|
| No `initialized` lines for the GPU, proofs on the CPU | `--gpus all` given? The NVIDIA Container Toolkit installed (`docker info \| grep -i nvidia`)? `docker exec midnight-proof-server nvidia-smi` sees the GPU? |
| `GPU-FALLBACK` lines | the log line names the operation and the error; a GPU out of memory runs that step on the CPU. Fewer workers or `--params-cache-mib 0` lower the memory in use |
| `autotune --show` exits with 2 | no calibration in effect: no GPU visible to the container, or `MIDNIGHT_AUTOTUNE=0` |
| HTTP 429 from `/prove` | the job queue is full: raise `--job-capacity` or the workers |
| `/ready` answers 503 | all workers busy and the queue full; it answers 200 again when jobs finish |
| Slow first start | the parameters and keys are downloading (empty parameters volume) |
| `nvidia-smi` on the host fails | a host problem (driver, power, PCIe); the container cannot fix it. Restart the host |

## What is in the image

```bash
docker inspect --format '{{json .Config.Labels}}' ricardomnf/midnight-proof-server-gpu:9.0.2
docker run --rm --entrypoint cat ricardomnf/midnight-proof-server-gpu:9.0.2 /opt/midnight-gpu/BUILD_MANIFEST
docker run --rm --entrypoint cat ricardomnf/midnight-proof-server-gpu:9.0.2 /opt/midnight-gpu/backend/BUILD_INFO
```

The labels name the commits the image was built from (this repository, the CUDA backend and both
proof-system forks); `BUILD_MANIFEST` lists the platform, the pinned dependencies and the compilers;
`BUILD_INFO` lists the backend's build options and the GPU architectures its libraries carry. How the
image is built: [README.md](README.md).
