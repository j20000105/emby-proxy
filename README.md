# emby-proxy

Rust 流式反向代理，默认回源 `https://iris.pvq.cc`。未知或缺少 Host
也使用默认回源，`wcdn.a.puq.li` 无需单独配置。

## 使用

Linux x86_64、systemd 主机可直接安装最新 GitHub Release：

```sh
curl -fsSL https://raw.githubusercontent.com/j20000105/emby-proxy/main/install.sh -o /tmp/emby-proxy-install.sh
sudo bash /tmp/emby-proxy-install.sh install
```

安装会提示填写默认代理目标地址，按回车使用 `https://iris.pvq.cc`，默认
监听 80。也可通过 `install --upstream https://example.com --listen 0.0.0.0:80`
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

从源码运行：

```sh
cargo build --release --locked
./target/release/emby-proxy --config examples/proxy.toml --check-config
./target/release/emby-proxy --config examples/proxy.toml
```

配置支持监听地址、按域名匹配、请求体上限、连接/读取超时和日志目录。
请求体默认每个最多 16 MiB，并用 `max_buffered_request_bytes` 限制总计
64 MiB 的并发请求体预留量；超出时返回 503，空请求和视频响应不占此预算。
生产环境使用 80 端口，TLS 在回源连接上校验，本程序入口为 HTTP。
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
sudo emby-proxy default https://iris.pvq.cc     # 修改默认回源
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

可热重载：默认回源、域名映射、跳转次数、连接/读取超时、单请求体大小、
内网回源策略。超时范围为 1–86400 秒。
`listen`、`log_directory`、`log_retention_days`、`max_buffered_request_bytes`、
`control_socket` 在启动时建立资源，修改后必须 `systemctl restart emby-proxy`；
此时 `reload` 会拒绝整份变更。服务启动前也会校验配置。

默认控制 socket 为 `/run/emby-proxy/control.sock`，权限 `0600`，仅 root 或
服务自身用户可以调用，没有网络管理端口。自定义多个实例应使用不同的绝对
socket 路径和日志目录；本地开发需配置可写路径。`reload --socket PATH` 可
显式选择旧 socket 取得校验结果，仍会检查配置文件是否属于目标进程。

## 转发行为

- 在服务器内跟随 301、302、303、307、308，最多 10 跳，循环返回 502。
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
python3 tests/management.py target/debug/emby-proxy
bash tests/installer.sh
```

GitHub Actions 对 main 推送和 PR 自动验证并打包静态 Linux x86_64 二进制。
推送 `v*` 标签会发布带 SHA256 校验文件的 GitHub Release。
部署配置、服务器清单与部署记录由
[iris-server/reverse-proxy](https://github.com/j20000105/iris-server/tree/main/reverse-proxy)
维护，本仓库只维护代理程序、示例配置、测试和构建。

首次部署的测试与审查证据见 [docs/reviews.md](docs/reviews.md)。
