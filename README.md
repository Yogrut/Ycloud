# Ycloud

轻量级自托管文件管理服务。后端为 Rust、Tokio、Axum，前端为 Vue 3.5、TypeScript。设计权重：安全 40%、性能 30%、稳定 30%。

## 功能

- 本地存储与 S3 兼容存储：阿里云 OSS、腾讯云 COS、MinIO/RustFS、通用 S3。
- 上传、下载、预览、搜索、归档、移动、复制、重命名和删除。
- 管理员、网页访客、普通账号和独立 WebDAV 权限。
- 文件夹锁、登录限流、IP 封禁、访问日志和传输限制。
- 大文件流式传输、本地原子写入和事务恢复。

## 启动

```powershell
cd D:\SYStemFiles\文档\open_code\Ycloud-v2
cargo run --release --locked
```

默认地址：`http://127.0.0.1:18473`。首次启动凭据写入 `initial-credentials.json`。

局域网测试：

```powershell
$env:BIND_ADDRESS = "0.0.0.0"
$env:PORT = "18473"
$env:ALLOW_LAN_HTTP = "true"
$env:SECURE_COOKIES = "false"
cargo run --release --locked
```

不要把局域网明文模式直接暴露到公网。公网部署使用可信 HTTPS 反向代理，并配置 `PUBLIC_BASE_URL`、`TRUSTED_PROXY_IPS` 和安全 Cookie。

## 检查

```powershell
cd frontend
npm ci
npm run check

cd ..
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
```

生产前端已内嵌到 Rust 程序，日常启动不需要 Node.js 或第二个终端。

结构和安全边界见 [ARCHITECTURE.md](ARCHITECTURE.md)，维护状态见 [HANDOFF.md](HANDOFF.md)。
