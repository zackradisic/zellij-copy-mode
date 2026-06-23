#!/usr/bin/env bash
# Dev loop: rebuild the plugin and hot-reload it into the running Zellij session
# (no new session needed). Run this from inside your Zellij session after editing.
set -euo pipefail

cd "$(dirname "$0")"
WASM="file:$PWD/target/wasm32-wasip1/release/zellij-copy-mode.wasm"

echo "building..."
cargo build --release --target wasm32-wasip1

echo "reloading $WASM"
# start-or-reload-plugin reloads the plugin module from disk, busting the
# server's in-memory cache that otherwise keeps serving the old build.
zellij action start-or-reload-plugin "$WASM"

echo "done — trigger copy mode again to use the new build"
