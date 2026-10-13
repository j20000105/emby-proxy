#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
git diff --quiet
git diff --cached --quiet
target=${1:-x86_64-unknown-linux-musl}
[[ "$target" == x86_64-unknown-linux-musl || "$target" == aarch64-unknown-linux-musl ]]
binary="target/$target/release/emby-proxy"
test -x "$binary"
mkdir -p dist
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
install -m 0755 "$binary" "$stage/emby-proxy"
install -m 0644 examples/proxy.toml "$stage/proxy.example.toml"
git rev-parse HEAD > "$stage/SOURCE_COMMIT"
cp Cargo.lock "$stage/Cargo.lock"
tar -C "$stage" -czf "dist/emby-proxy-$target.tar.gz" emby-proxy proxy.example.toml SOURCE_COMMIT Cargo.lock
(cd dist && sha256sum "emby-proxy-$target.tar.gz" > "emby-proxy-$target.tar.gz.sha256")
