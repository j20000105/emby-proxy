# emby-proxy

Rust 流式反向代理，默认回源 `https://iris.pvq.cc`。未知或缺少 Host
也使用默认回源，`wcdn.a.puq.li` 无需单独配置。

## 使用

```sh
cargo build --release --locked
./target/release/emby-proxy --config examples/proxy.toml --check-config
./target/release/emby-proxy --config examples/proxy.toml
```

配置支持监听地址、按域名匹配、请求体上限、连接/读取超时和日志目录。
生产环境使用 80 端口，TLS 在回源连接上校验，本程序入口为 HTTP。
独立域名回源通过 TOML `[routes]` 配置；忽略 Host 大小写和端口。
程序不使用环境变量中的 HTTP 代理。

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
```

GitHub Actions 对 main 推送和 PR 自动验证并打包静态 Linux x86_64 二进制。
推送 `v*` 标签会发布带 SHA256 校验文件的 GitHub Release。
部署配置、服务器清单与部署记录由
[iris-server/reverse-proxy](https://github.com/j20000105/iris-server/tree/main/reverse-proxy)
维护，本仓库只维护代理程序、示例配置、测试和构建。

首次部署的测试与审查证据见 [docs/reviews.md](docs/reviews.md)。
