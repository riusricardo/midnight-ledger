#!/usr/bin/env bash
# Builds the GPU proof server image (proof-server/docker/gpu.Dockerfile) for each platform into the
# local Docker image store.
#
# usage: proof-server/docker/build-gpu-image.sh --backend <checkout of midnight-bls12-381-cuda>
#          [--platforms linux/amd64,linux/arm64] [--tag <name>] [--progress plain]
#
# Each platform's image is tagged <name>:<version>-<arch> (amd64, arm64), and the build machine's own
# platform also <name>:<version>, <version> being the proof server's (default name: proof-server-gpu).
# As in upstream's release, the multi-platform <name>:<version> is assembled in the registry from the
# pushed per-platform tags (`docker buildx imagetools create`); the local image store holds one
# platform per tag. Every platform compiles on the build machine (arm64 is cross-compiled), so no
# emulation is needed.
#
# The backend repository is private: instead of fetching it from GitHub, the build takes its git
# history from a bare clone of the given checkout (it must hold the commit Cargo.lock pins), passed
# as the build context `backend-git`.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
backend=""
tag=proof-server-gpu
platforms=linux/amd64,linux/arm64
progress=auto
while [ $# -gt 0 ]; do
  case "$1" in
    --backend) backend=${2:?}; shift 2 ;;
    --platforms) platforms=${2:?}; shift 2 ;;
    --tag) tag=${2:?}; shift 2 ;;
    --progress) progress=${2:?}; shift 2 ;;
    *) echo "usage: $0 --backend <dir> [--platforms <list>] [--tag <name>] [--progress plain]" >&2; exit 2 ;;
  esac
done
[ -n "$backend" ] || { echo "$0: --backend <checkout of midnight-bls12-381-cuda> is required" >&2; exit 2; }

if [ -n "$(git -C "$root" status --porcelain)" ]; then
  echo "$0: the ledger tree has local changes; the image is built from committed sources only" >&2
  exit 1
fi
locked() { # <package name> [<version>]: the commit Cargo.lock pins for a git package
  awk -v n="$1" -v v="${2:-}" '
    $0 == "[[package]]" { name = ""; ver = "" }
    $1 == "name" { name = $3 }
    $1 == "version" { ver = $3 }
    $1 == "source" && name == "\"" n "\"" && (v == "" || ver == "\"" v "\"") {
      sub(/.*#/, ""); sub(/"$/, ""); print; exit }' "$root/Cargo.lock"
}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/proof-server/Cargo.toml" | head -1)
backend_rev=$(locked midnight-bls12-381-cuda)
if ! git -C "$backend" merge-base --is-ancestor "$backend_rev" main 2>/dev/null; then
  echo "$0: $backend's main does not contain the pinned backend commit $backend_rev; fetch it" >&2
  exit 1
fi
native=$(docker version --format '{{.Server.Os}}/{{.Server.Arch}}')

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
git clone --quiet --bare --single-branch --branch main "$backend" "$tmp/backend.git"

for platform in ${platforms//,/ }; do
  arch=${platform#*/}
  tags=(-t "$tag:$version-$arch")
  [ "$platform" = "$native" ] && tags+=(-t "$tag:$version")
  echo "== $platform"
  docker buildx build --load --platform "$platform" --progress "$progress" \
    -f "$here/gpu.Dockerfile" \
    --build-context backend-git="$tmp/backend.git" \
    --build-arg VERSION="$version" \
    --build-arg LEDGER_COMMIT="$(git -C "$root" rev-parse HEAD)" \
    --build-arg BACKEND_COMMIT="$backend_rev" \
    --build-arg PROOFS_V1_COMMIT="$(locked midnight-proofs 0.7.3)" \
    --build-arg PROOFS_V2_COMMIT="$(locked midnight-proofs 0.8.2)" \
    "${tags[@]}" \
    "$root"
  echo "built $tag:$version-$arch (backend $backend_rev)"
done
