#!/usr/bin/env bash
# Public entry point: sudo bash install.sh install | update
set -euo pipefail
umask 022

REPOSITORY=j20000105/emby-proxy
BIN_ROOT=/usr/local/lib/emby-proxy
CONFIG_ROOT=/etc/emby-proxy
LOG_ROOT=/var/log/emby-proxy
UNIT=/etc/systemd/system/emby-proxy.service
READY_DELAY=1
WORK=''
ROLLBACK_NEEDED=false
MODE=''
PREVIOUS=''

die() { printf '%s\n' "$*" >&2; return 1; }

download() {
  curl --proto '=https' --proto-redir '=https' --tlsv1.2 -fsSL --retry 3 "$@"
}

ready() {
  local _attempt
  for _attempt in {1..8}; do
    sleep "$READY_DELAY"
    if systemctl is-active --quiet emby-proxy.service; then return 0; fi
  done
  return 1
}

point_to() {
  ln -sfn "$1" "$BIN_ROOT/.current.$$" || return 1
  mv -Tf "$BIN_ROOT/.current.$$" "$BIN_ROOT/current"
}

finish() {
  local status=$1
  trap - EXIT
  if [[ "$status" != 0 ]] && $ROLLBACK_NEEDED; then
    if [[ -n "$PREVIOUS" ]]; then
      if point_to "$PREVIOUS" && systemctl restart emby-proxy.service && ready; then
        printf '更新失败，已恢复上一版本。\n' >&2
      else
        printf '更新失败，旧版本已恢复，但服务未恢复健康，请检查 journalctl -u emby-proxy。\n' >&2
      fi
    else
      systemctl disable --now emby-proxy.service >/dev/null 2>&1 || true
      rm -f "$BIN_ROOT/current" "$CONFIG_ROOT/proxy.toml" "$UNIT"
      systemctl daemon-reload || true
      printf '安装未成功，已撤回本次创建的配置和服务。\n' >&2
    fi
  fi
  [[ -z "$WORK" ]] || rm -rf "$WORK"
  rm -f "$BIN_ROOT/.current.$$"
  exit "$status"
}

fetch_release() {
  local tag checksum members extraction
  download "https://api.github.com/repos/$REPOSITORY/releases/latest" -o "$WORK/release.json"
  tag=$(jq -er '.tag_name' "$WORK/release.json")
  [[ "$tag" =~ ^v[0-9A-Za-z._-]+$ ]] || die 'GitHub 返回了无效版本号。'
  local name=emby-proxy-x86_64-unknown-linux-musl.tar.gz
  local url="https://github.com/$REPOSITORY/releases/download/$tag"
  download "$url/$name" -o "$WORK/$name"
  download "$url/$name.sha256" -o "$WORK/$name.sha256"
  read -r checksum _ < "$WORK/$name.sha256"
  [[ "$checksum" =~ ^[0-9a-f]{64}$ ]] || die '无效的 SHA256 校验文件。'
  printf '%s  %s\n' "$checksum" "$WORK/$name" | sha256sum -c -
  members=$(tar -tzf "$WORK/$name" | LC_ALL=C sort)
  [[ "$members" == $'Cargo.lock\nSOURCE_COMMIT\nemby-proxy\nproxy.example.toml' ]] || die '发布包内容与预期不符。'
  extraction="$WORK/extracted"
  mkdir "$extraction"
  tar --no-same-owner -xzf "$WORK/$name" -C "$extraction"
  SOURCE_COMMIT=$(cat "$extraction/SOURCE_COMMIT")
  [[ "$SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ ]] || die '发布包缺少有效源码版本。'
  CANDIDATE="$BIN_ROOT/releases/$SOURCE_COMMIT"
  mkdir -p "$BIN_ROOT/releases"
  if [[ -e "$CANDIDATE" ]]; then
    cmp "$extraction/emby-proxy" "$CANDIDATE/emby-proxy" || die '同一源码版本对应的已安装程序不一致。'
  else
    # Rename within the release filesystem; never overwrite a running binary.
    local staged
    staged=$(mktemp -d "$BIN_ROOT/releases/.install.XXXXXXXX")
    install -m 0755 "$extraction/emby-proxy" "$staged/emby-proxy"
    install -m 0644 "$extraction/SOURCE_COMMIT" "$staged/SOURCE_COMMIT"
    chmod 0755 "$staged"
    mv "$staged" "$CANDIDATE"
  fi
  printf '最新版本：%s（%s）\n' "$tag" "$SOURCE_COMMIT"
}

write_unit() {
  cat > "$WORK/emby-proxy.service" <<'UNIT'
[Unit]
Description=Emby streaming reverse proxy
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
DynamicUser=yes
ExecStartPre=/usr/local/lib/emby-proxy/current/emby-proxy --config /etc/emby-proxy/proxy.toml --check-config
ExecStart=/usr/local/lib/emby-proxy/current/emby-proxy --config /etc/emby-proxy/proxy.toml
Restart=on-failure
RestartSec=3
TimeoutStopSec=20
LogsDirectory=emby-proxy
LogsDirectoryMode=0750
UMask=0027
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE
NoNewPrivileges=yes
PrivateTmp=yes
PrivateDevices=yes
ProtectSystem=strict
ProtectHome=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
RestrictNamespaces=yes
LockPersonality=yes
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
UNIT
  install -m 0644 "$WORK/emby-proxy.service" "$UNIT"
}

activate() {
  local upstream=$1 listen=$2
  if [[ "$MODE" == install ]]; then
    printf 'listen = "%s"\ndefault_upstream = "%s"\nlog_directory = "%s"\nlog_retention_days = 30\n' \
      "$listen" "$upstream" "$LOG_ROOT" > "$WORK/proxy.toml"
    "$CANDIDATE/emby-proxy" --config "$WORK/proxy.toml" --check-config
    ROLLBACK_NEEDED=true
    mkdir -p "$CONFIG_ROOT" "$(dirname "$UNIT")"
    install -m 0644 "$WORK/proxy.toml" "$CONFIG_ROOT/proxy.toml"
    write_unit
    systemctl daemon-reload
    systemctl enable emby-proxy.service
  else
    "$CANDIDATE/emby-proxy" --config "$CONFIG_ROOT/proxy.toml" --check-config
    ROLLBACK_NEEDED=true
  fi
  point_to "$CANDIDATE"
  systemctl restart emby-proxy.service
  ready || die '新版本服务未正常运行。'
  ROLLBACK_NEEDED=false
  printf '完成。配置：%s/proxy.toml；日志：%s\n' "$CONFIG_ROOT" "$LOG_ROOT"
}

main() {
  MODE=${1:-}
  [[ "$MODE" == install || "$MODE" == update ]] || die '用法：sudo bash install.sh install [--upstream https://example.com] [--listen 0.0.0.0:80] | update'
  shift
  local upstream='' listen=0.0.0.0:80
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --upstream) [[ $# -ge 2 ]] || die '--upstream 缺少值'; upstream=$2; shift 2 ;;
      --listen) [[ $# -ge 2 ]] || die '--listen 缺少值'; listen=$2; shift 2 ;;
      *) die "未知参数：$1" ;;
    esac
  done
  [[ $EUID -eq 0 ]] || die '请使用 root 或 sudo 运行。'
  [[ $(uname -s) == Linux && $(uname -m) == x86_64 ]] || die '目前发布包支持 Linux x86_64。'
  [[ -d /run/systemd/system ]] || die '需要使用 systemd 的 Linux 主机。'
  exec 9>/run/lock/emby-proxy-install.lock
  flock -n 9 || die '另一个安装或更新操作正在运行。'
  if [[ "$MODE" == install ]]; then
    [[ ! -e "$CONFIG_ROOT/proxy.toml" && ! -e "$UNIT" && ! -e "$BIN_ROOT/current" && ! -L "$BIN_ROOT/current" ]] || die '检测到已有安装，请使用 update；不会覆盖现有配置。'
    if [[ -z "$upstream" ]]; then
      read -r -p '默认代理目标地址 [https://iris.pvq.cc]：' upstream </dev/tty || die '无交互终端，请通过 --upstream 指定地址。'
      upstream=${upstream:-https://iris.pvq.cc}
    fi
    local origin_pattern='^https?://(\[[0-9A-Fa-f:]+\]|[A-Za-z0-9.-]+)(:[0-9]{1,5})?/?$'
    [[ "$upstream" =~ $origin_pattern ]] || die '请输入 HTTP(S) 源站地址，可含端口，不包含路径、凭据或查询参数。'
    local listen_pattern='^([0-9.]+|\[[0-9A-Fa-f:]+\]):[0-9]+$'
    [[ "$listen" =~ $listen_pattern ]] || die '监听地址必须为 IP:端口。'
  else
    [[ -z "$upstream" && "$listen" == 0.0.0.0:80 ]] || die 'update 保留原配置，不接受配置参数。'
    [[ -f "$CONFIG_ROOT/proxy.toml" && -f "$UNIT" && -L "$BIN_ROOT/current" ]] || die '未找到完整安装，请先安装。'
    PREVIOUS=$(readlink -f "$BIN_ROOT/current")
    [[ -x "$PREVIOUS/emby-proxy" ]] || die '当前版本不存在，停止更新。'
  fi
  if ! command -v curl >/dev/null || ! command -v jq >/dev/null; then
    command -v apt-get >/dev/null || die '请先安装 curl、jq、ca-certificates。'
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends curl jq ca-certificates
  fi
  WORK=$(mktemp -d)
  trap 'finish "$?"' EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  fetch_release
  activate "$upstream" "$listen"
}

if [[ -z "${BASH_SOURCE[0]:-}" || "${BASH_SOURCE[0]}" == "$0" ]]; then main "$@"; fi
