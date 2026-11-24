#!/usr/bin/env bash
# Builds the GPU proof server image (proof-server/docker/gpu.Dockerfile) into the local Docker
# image store.
#
# usage: proof-server/docker/build-gpu-image.sh --backend <checkout of midnight-bls12-381-cuda>
#                                               [--tag <name>] [--progress plain]
#
# The backend repository is private: instead of fetching it from GitHub, the build takes its git
# history from a bare clone of the given checkout (it must hold the commit Cargo.lock pins), passed
# as the build context `backend-git`. The image is tagged <name>:<version> and
# <name>:<version>-amd64, <version> being the proof server's (default name: proof-server-gpu).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
backend=""
tag=proof-server-gpu
progress=auto
while [ $# -gt 0 ]; do
  case "$1" in
    --backend) backend=${2:?}; shift 2 ;;
    --tag) tag=${2:?}; shift 2 ;;
    --progress) progress=${2:?}; shift 2 ;;
    *) echo "usage: $0 --backend <dir> [--tag <name>] [--progress plain]" >&2; exit 2 ;;
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

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
git clone --quiet --bare --single-branch --branch main "$backend" "$tmp/backend.git"

docker buildx build --load --progress "$progress" \
  -f "$here/gpu.Dockerfile" \
  --build-context backend-git="$tmp/backend.git" \
  --build-arg VERSION="$version" \
  --build-arg LEDGER_COMMIT="$(git -C "$root" rev-parse HEAD)" \
  --build-arg BACKEND_COMMIT="$backend_rev" \
  --build-arg PROOFS_V1_COMMIT="$(locked midnight-proofs 0.7.3)" \
  --build-arg PROOFS_V2_COMMIT="$(locked midnight-proofs 0.8.2)" \
  -t "$tag:$version" -t "$tag:$version-amd64" \
  "$root"
echo "built $tag:$version (backend $backend_rev)"
