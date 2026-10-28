#!/usr/bin/env bash
# Build command for Cloudflare Workers Builds (Worker "standard-grow"), so
# deploys don't depend on GitHub Actions minutes. The build image has Node but
# no Rust: install a minimal toolchain (a pinned rustup-init, checked against
# the digest below) and a pinned worker-build, then compile. Cloudflare's
# deploy command then runs
#   npx wrangler d1 migrations apply standard-grow --remote && npx wrangler deploy
# with the build variable WORKER_PREBUILT=1 (see wrangler.toml).
set -euo pipefail
cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"
RUST=1.93.0
RUSTUP=1.28.2
RUSTUP_SHA256=20a06e644b0d9bd2fbdbfd52d42540bdde820ea7df86e92e533c073da0cdd43c
WORKER_BUILD=0.8.7
if ! command -v rustup >/dev/null; then
  init="$(mktemp)"
  curl --proto '=https' --tlsv1.2 -sSf -o "$init" \
    "https://static.rust-lang.org/rustup/archive/$RUSTUP/x86_64-unknown-linux-gnu/rustup-init"
  echo "$RUSTUP_SHA256  $init" | sha256sum -c -
  chmod +x "$init"
  "$init" -y --profile minimal --default-toolchain "$RUST"
fi
rustup toolchain install "$RUST" --profile minimal --target wasm32-unknown-unknown
if [ "$(worker-build --version 2>/dev/null || true)" != "$WORKER_BUILD" ]; then
  cargo +"$RUST" install worker-build --version "$WORKER_BUILD" --locked --force
fi
RUSTUP_TOOLCHAIN="$RUST" worker-build --release
