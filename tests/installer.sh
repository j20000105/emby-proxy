#!/usr/bin/env bash
# Exercise the real release/config/rollback functions in an isolated directory.
# Download and systemd are replaced with deterministic boundary fixtures only.
set -euo pipefail
cd "$(dirname "$0")/.."
export INSTALLER_SOURCE="$PWD/install.sh"
TEST_DIRECTORY=$(mktemp -d)
export TEST_DIRECTORY
trap 'rm -rf "$TEST_DIRECTORY"' EXIT
name=emby-proxy-x86_64-unknown-linux-musl.tar.gz
for version in 1 2 3; do
  directory="$TEST_DIRECTORY/fixture-$version"
  mkdir -p "$directory/content"
  printf '%040d\n' "$version" > "$directory/content/SOURCE_COMMIT"
  printf 'fixture\n' > "$directory/content/Cargo.lock"
  printf 'fixture\n' > "$directory/content/proxy.example.toml"
  printf '#!/usr/bin/env bash\n# version %s\nexit 0\n' "$version" > "$directory/content/emby-proxy"
  if [[ "$version" == 3 ]]; then echo '# fail-to-serve' >> "$directory/content/emby-proxy"; fi
  chmod +x "$directory/content/emby-proxy"
  tar -C "$directory/content" -czf "$directory/$name" emby-proxy SOURCE_COMMIT Cargo.lock proxy.example.toml
  (cd "$directory" && sha256sum "$name" > "$name.sha256")
done
cat > "$TEST_DIRECTORY/phase.sh" <<'PHASE'
#!/usr/bin/env bash
set -euo pipefail
# shellcheck source=../install.sh
source "$INSTALLER_SOURCE"
BIN_ROOT="$TEST_DIRECTORY/bin"
CONFIG_ROOT="$TEST_DIRECTORY/config"
LOG_ROOT="$TEST_DIRECTORY/logs"
UNIT="$TEST_DIRECTORY/systemd/emby-proxy.service"
READY_DELAY=0
MODE=$1
version=$2
download() {
  local url=$1
  [[ "$2" == -o ]]
  if [[ "$url" == */latest ]]; then
    printf '{"tag_name":"v0.1.0"}\n' > "$3"
  else
    cp "$TEST_DIRECTORY/fixture-$version/${url##*/}" "$3"
  fi
}
systemctl() {
  if [[ "$1" == is-active ]]; then
    ! grep -q fail-to-serve "$BIN_ROOT/current/emby-proxy"
  fi
}
if [[ "$MODE" == update ]]; then PREVIOUS=$(readlink -f "$BIN_ROOT/current"); fi
WORK=$(mktemp -d "$TEST_DIRECTORY/work.XXXXXXXX")
trap 'finish "$?"' EXIT
fetch_release
activate https://fixture.example 127.0.0.1:18080
PHASE
bash "$TEST_DIRECTORY/phase.sh" install 1 >/dev/null
grep -Fq 'default_upstream = "https://fixture.example"' "$TEST_DIRECTORY/config/proxy.toml"
mkdir -p "$TEST_DIRECTORY/logs"
echo 'keep my logs' > "$TEST_DIRECTORY/logs/existing"
echo '# operator customization' >> "$TEST_DIRECTORY/config/proxy.toml"
cp "$TEST_DIRECTORY/config/proxy.toml" "$TEST_DIRECTORY/expected-config"
bash "$TEST_DIRECTORY/phase.sh" update 2 >/dev/null
[[ $(cat "$TEST_DIRECTORY/bin/current/SOURCE_COMMIT") == "$(printf '%040d' 2)" ]]
cmp "$TEST_DIRECTORY/expected-config" "$TEST_DIRECTORY/config/proxy.toml"
grep -Fq 'keep my logs' "$TEST_DIRECTORY/logs/existing"
if bash "$TEST_DIRECTORY/phase.sh" update 3 >"$TEST_DIRECTORY/failure.log" 2>&1; then
  echo 'Expected failed activation' >&2; exit 1
fi
[[ $(cat "$TEST_DIRECTORY/bin/current/SOURCE_COMMIT") == "$(printf '%040d' 2)" ]]
cmp "$TEST_DIRECTORY/expected-config" "$TEST_DIRECTORY/config/proxy.toml"
printf tampered >> "$TEST_DIRECTORY/fixture-1/$name"
if bash "$TEST_DIRECTORY/phase.sh" update 1 >"$TEST_DIRECTORY/failure.log" 2>&1; then
  echo 'Expected checksum rejection' >&2; exit 1
fi
[[ $(cat "$TEST_DIRECTORY/bin/current/SOURCE_COMMIT") == "$(printf '%040d' 2)" ]]
printf 'PASS: installer, configuration preservation, update, failed-start rollback and checksum rejection\n'
