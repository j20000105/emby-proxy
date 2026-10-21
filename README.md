# emby-proxy

Rust 流式反向代理。**不内置任何回源地址**，必须在配置文件中显式填写
`default_upstream`。未知或缺少 Host 时使用这个已配置的回源。
文中的 `example.com`、`example.net` 及其子域名均为示例，请替换为自己的地址。

## 使用

Linux x86_64、systemd 主机可直接安装最新 GitHub Release：

```sh
curl -fsSL https://raw.githubusercontent.com/j20000105/emby-proxy/main/install.sh -o /tmp/emby-proxy-install.sh
sudo bash /tmp/emby-proxy-install.sh install
```

安装会提示填写代理目标地址，**必填且无默认值**，空输入会报错退出。
默认监听 80。也可通过 `install --upstream https://example.com --listen 0.0.0.0:80`
无交互安装。目标为 HTTP(S) 源站地址，不带路径、查询参数或密码。
安装前需释放监听端口；脚本不会卸载其他代理程序。

更新到最新 Release 并重启（保留全部现有配置和日志）：

```sh
curl -fsSL https://raw.githubusercontent.com/j20000105/emby-proxy/main/install.sh -o /tmp/emby-proxy-install.sh
sudo bash /tmp/emby-proxy-install.sh update
```

脚本核对 SHA256，并在新程序通过配置检查后原子切换 `current`。
新服务启动失败时恢复旧二进制并重启。配置位于
`/etc/emby-proxy/proxy.toml`，版本目录为
`/usr/local/lib/emby-proxy/releases/<源码提交号>`，日志位于
`/var/log/emby-proxy/`。命令入口为 `/usr/local/bin/emby-proxy`。
更新保留配置和原 systemd 单元，通过受管的
`emby-proxy.service.d/10-emby-proxy-runtime.conf` 添加重载命令及运行目录；
失败时同时恢复这个文件。应用未来若需要配置迁移，配置校验失败会在切换前
终止。旧版本保留供回滚。

从源码运行，先复制配置模板并填写自己的回源地址：

```sh
cargo build --release --locked
cp examples/proxy.toml proxy.toml
# 编辑 proxy.toml，填写 default_upstream，并设置可写的日志和控制 socket 路径。
./target/release/emby-proxy --config proxy.toml --check-config
./target/release/emby-proxy --config proxy.toml
```

没有指定 `--config` 时读取 `/etc/emby-proxy/proxy.toml`。文件缺失、
`default_upstream` 缺失或为空时，程序在监听端口前以非零状态退出，
标准错误日志会提示缺失配置；systemd 安装可用 `journalctl -u emby-proxy`
查看。配置有效后才初始化滚动日志。`--check-config` 执行相同的校验。
模板中的回源值故意留空，不可直接用于启动；已填写回源的现有配置仍可使用。

配置支持监听地址、按域名匹配、请求体上限、连接/读取超时和日志目录。
请求体默认每个最多 16 MiB，并用 `max_buffered_request_bytes` 限制总计
64 MiB 的并发请求体预留量；超出时返回 503，空请求和视频响应不占此预算。
监听端口可配置，TLS 在回源连接上校验，本程序入口为 HTTP。
独立域名回源通过 TOML `[routes]` 配置；忽略 Host 大小写和端口。
程序不使用环境变量中的 HTTP 代理。

## 配置管理与重载

命令默认操作 `/etc/emby-proxy/proxy.toml`，可用 `--config FILE` 指定其他文件。
配置管理不依赖服务运行，只修改磁盘文件，**不会自动 reload**：

```sh
sudo emby-proxy route list
sudo emby-proxy route get example.com
sudo emby-proxy route add example.com https://origin.example.com
sudo emby-proxy route update example.com https://new-origin.example.com
sudo emby-proxy route delete example.com
sudo emby-proxy default                        # 查询默认回源
sudo emby-proxy default https://origin.example.com     # 修改默认回源
```

`list` 输出包含默认回源和显式映射的 JSON。域名精确匹配，忽略大小写、尾部
单个句点和有效端口；不支持通配符。`add` 要求域名不存在，`update/delete/get`
要求已有显式映射。修改前后都会完整校验 TOML，保留未删除项的注释、文件权限
和所有者，以文件锁串行处理并发写入，再原子替换文件。命令拒绝修改符号链接，
可通过 `--config` 指定其真实目标；手动编辑器也应采用原子保存。

修改完成后手动应用：

```sh
sudo emby-proxy --check-config   # 可选：单独检查文件
sudo emby-proxy reload           # 校验成功后才重载；等待服务确认
# 或 sudo systemctl reload emby-proxy
```

`reload` 先校验 TOML 格式、未知字段、域名冲突、回源 URL 和数值范围，再通过
本机 Unix socket 请求运行进程重复校验、构建新回源客户端并一次性切换。
成功后新请求使用新配置，已有视频流和 WebSocket 继续运行，进程 PID 不变。
任何校验失败都会返回非零退出码并保留整份旧运行配置，不会只应用部分改动。
查询命令显示磁盘配置；保存文件后尚未 reload 的内容不会影响运行中的路由。

可热重载：默认回源、域名映射、跳转次数、重定向域名策略、连接/读取超时、单请求体大小、
内网回源策略。超时范围为 1–86400 秒。
`listen`、`log_directory`、`log_retention_days`、`max_buffered_request_bytes`、
`control_socket` 在启动时建立资源，修改后必须 `systemctl restart emby-proxy`；
此时 `reload` 会拒绝整份变更。服务启动前也会校验配置。

默认控制 socket 为 `/run/emby-proxy/control.sock`，权限 `0600`，仅 root 或
服务自身用户可以调用，没有网络管理端口。自定义多个实例应使用不同的绝对
socket 路径和日志目录；本地开发需配置可写路径。`reload --socket PATH` 可
显式选择旧 socket 取得校验结果，仍会检查配置文件是否属于目标进程。

## 转发行为

- 在 TOML 顶层（`[routes]` 之前）设置
  `redirect_follow_domains = ["example.com"]`：仅在重定向目标为该域名或其
  任意层级子域名时，由服务器跟随并转发内容。其他目标直接返回原始 3xx
  状态、签名 `Location` 和流式响应体，不请求该目标。
  策略适用于 301、302、303、307、308，每一跳重新判断；最多跟随 10 跳，
  跟随链循环返回 502。设为 `[]` 则所有重定向都交给客户端。
  匹配忽略域名大小写、单个末尾句点和目标 URL 端口，按完整 DNS 标签匹配，
  不会命中 `example.net` 或 `example.com.example.net`。只检查 Location 主机名，
  不检查 DNS CNAME；`cdn.example.net` 等其他域名都直接返回。
  配置项只接受域名，不接受 URL、IP、端口或 `*.` 通配符。
  绝对签名地址原样保留，相对地址按上游 URL 转成绝对地址。
  WebSocket 握手仍不跟随或直传重定向；修改后执行 `emby-proxy reload` 生效。
  为兼容旧配置，省略 `redirect_follow_domains` 时保留原有行为：默认全部
  跟随，或按 v0.3 的 `redirect_passthrough_domains` 排除指定域名。
  新列表不能与非空旧列表同时配置；迁移时删除旧项并写入新项。
- 视频响应逐块转发，保留 Range、Content-Range、Content-Length、HEAD
  和压缩字节；不对整个视频进行缓冲。WebSocket 双向转发。
- 跨站重定向只保留播放/缓存协商所需头，不转发 Emby 令牌、Cookie
  或 Authorization；跨站保留非空请求体的跳转被拒绝。
- 默认禁止回源内网、回环、链路本地及特殊用途地址，连接使用经过过滤的
  DNS 结果。`allow_private_upstreams = true` 仅供可信内网和本地测试。
- 读取超时作用于等待上游数据的空闲阶段，不限制视频总时长。
- JSON 日志按 UTC 日期滚动，启动和每小时清理过期日志，最多保留最近
  30 个日期。记录请求 ID、类型、状态、重定向次数、流量和时长，不记录
  查询参数、认证头或签名 URL。

## 验证与构建

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked
python3 tests/protocol.py target/debug/emby-proxy
python3 tests/follow_redirects.py target/debug/emby-proxy
python3 tests/management.py target/debug/emby-proxy
python3 tests/startup.py target/debug/emby-proxy
bash tests/installer.sh
```

真实播放校验支持同一跟随策略（fixture 从标准输入读取，不打印认证信息）：

```sh
python3 tests/live_playback.py --proxy http://127.0.0.1:18080 \
  --origin https://origin.example.com \
  --follow-domains example.com < /private/playback-fixture.json
```

该工具同时核对客户端收到的重定向策略，以及直连和代理路径的起播、拖动、
末尾各 64 KiB 内容和 HEAD。名单外的重定向以无认证头的请求读取 CDN；
名单内的重定向必须已由代理跟随。`--follow-domains` 可接多个域名，
不带域名表示全部直传。旧配置可继续使用 `--passthrough-domain` 校验。
`--origin` 为必填项，必须与待测代理配置的回源一致；它只改变校验器的直连入口，
不会修改代理回源配置。

GitHub Actions 对 main 推送和 PR 自动验证并打包静态 Linux x86_64 二进制。
推送 `v*` 标签会发布带 SHA256 校验文件的 GitHub Release。
部署配置、服务器清单与部署记录应放在独立的私有部署仓库中。
本仓库只维护代理程序、示例配置、测试和构建。

首次部署的测试与审查证据见 [docs/reviews.md](docs/reviews.md)。
