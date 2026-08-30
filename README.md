# Ycloud

轻量级自托管文件管理服务。后端为 Rust、Tokio、Axum，前端为 Vue 3.5、TypeScript。设计权重：安全 40%、性能 30%、稳定 30%。

## 功能

- 本地存储与 S3 兼容存储：阿里云 OSS、腾讯云 COS、MinIO/RustFS、通用 S3。
- 文件与文件夹上传、下载、预览、搜索、归档、移动、复制、重命名和删除。
- 管理员、网页访客、普通账号和独立 WebDAV 权限。
- 文件夹锁、管理员 TOTP（兼容 2FAuth）、登录限流、IP 封禁、访问日志和传输限制。
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

单文件、单批次上传和打包下载的业务上限可在管理后台调整。部署者可用 `MAX_UPLOAD_BYTES`、`MAX_UPLOAD_BATCH_BYTES`、`MAX_UPLOAD_BATCH_ENTRIES`、`MAX_ARCHIVE_BYTES`、`MAX_ARCHIVE_ENTRIES` 设置后台不可突破的绝对上限；并发、事务锁、密码验证队列和磁盘安全余量仍由服务固定保护。

多存储部署还需要按实际情况设置：

- `LOCAL_STORAGE_MOUNTS`：额外挂载点 JSON 数组，例如 `[{"id":"archive","name":"归档盘","path":"D:\\archive"}]`。后台只能从这里声明的目录中选择，不能随意输入服务器路径。
- `S3_ALLOWED_ENDPOINTS`：MinIO、RustFS 和通用 S3 Endpoint 精确白名单，多个 Origin 用逗号分隔；阿里 OSS 与腾讯 COS 必须使用与 Region 对应的官方 HTTPS Endpoint。
- 单机默认自动创建 `.ycloud-system/secrets/master.key`，用于加密 S3 SecretKey 和管理员 TOTP 密钥。Windows 使用当前用户的 DPAPI 保护；Unix 限制为 `0600`。必须随配置一起备份，丢失后加密配置无法恢复。
- Docker、集群和专业部署优先使用 `YCLOUD_CONFIG_KEY_FILE` 指向只读 Secret 文件；`YCLOUD_CONFIG_KEY` 环境变量保留为覆盖项。两者内容都是 32 个随机字节的 URL-safe Base64（无 `=`）。

PowerShell 生成配置主密钥：

```powershell
$keyBytes = [byte[]]::new(32)
[Security.Cryptography.RandomNumberGenerator]::Fill($keyBytes)
$configKey = [Convert]::ToBase64String($keyBytes).TrimEnd('=').Replace('+', '-').Replace('/', '_')
# 集群中把 $configKey 写入受保护的 Secret 文件，并设置 YCLOUD_CONFIG_KEY_FILE。
```

测试自建 RustFS 时还需例如：`$env:S3_ALLOWED_ENDPOINTS = "http://192.168.2.37:9000"`。测试连接会执行创建、读取、复制和删除临时对象，不只检查端口。

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

结构和安全边界见 [ARCHITECTURE.md](ARCHITECTURE.md)，维护结论见 [MAINTENANCE_REPORT.md](MAINTENANCE_REPORT.md)。
