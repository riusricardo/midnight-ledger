#!/bin/bash
# The GPU image's entrypoint, with upstream's image contract (flake.nix): PORT (default 6300) sets
# the server's port unless MIDNIGHT_PROOF_SERVER_PORT is given; no arguments, or arguments that
# start with `-`, run midnight-proof-server with them; one argument holding spaces runs as a shell
# command (the old `midnight-proof-server ...` form); anything else runs as given, so
# `docker run <image> midnight-proof-server autotune --show` reaches the GPU calibration command.
export PORT="${PORT:-6300}"
export MIDNIGHT_PROOF_SERVER_PORT="${MIDNIGHT_PROOF_SERVER_PORT:-$PORT}"
case "${1-}" in
  "" | -*) set -- midnight-proof-server "$@" ;;
  *" "*) exec bash -c "$*" ;;
esac
exec "$@"
