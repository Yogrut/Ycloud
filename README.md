# Ycloud

轻量级自托管文件管理服务。后端为 Rust、Tokio、Axum，前端为 Vue 3.5、TypeScript。设计权重：安全 40%、性能 30%、稳定 30%。

## 支持范围

- 正式生产部署目标为 Linux 原生进程、Docker 和 Docker Compose。
- Windows 仅用于开发、编译和本机功能测试；项目不提供 Windows 服务安装、生产 ACL/DACL 加固或生产部署支持。自行在 Windows 编译部署不属于官方发布验收范围。

## 功能

- 本地存储与 S3 兼容存储：阿里云 OSS、腾讯云 COS、MinIO/RustFS、通用 S3。
- 文件与文件夹上传、下载、预览、搜索、归档、移动、复制、重命名和删除。
- 管理员、网页访客、普通账号和独立 WebDAV 权限。
- 文件夹锁、管理员 TOTP（兼容 2FAuth）、登录限流、IP 封禁、访问日志和传输限制。
- 大文件流式传输、本地原子写入和事务恢复。

## Windows 开发运行

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

Host 边界默认只接受与运行模式一致的地址：回环模式接受 `localhost`/回环 IP，LAN 模式接受本地 IP，公网代理模式只接受 `PUBLIC_BASE_URL` 的 Host。使用自定义内网域名时，通过 `ALLOWED_HOSTS` 精确声明 Authority（含非默认端口），多个值用逗号分隔，例如 `ycloud.lan:18473`。后端监听回环地址时仍可启用公网 HTTPS 代理模式；只要设置了 `PUBLIC_BASE_URL` 或 `TRUSTED_PROXY_IPS`，两项就必须完整配置且 `SECURE_COOKIES=true`。

单文件、单批次上传和打包下载的业务上限可在管理后台调整。部署者可用 `MAX_UPLOAD_BYTES`、`MAX_UPLOAD_BATCH_BYTES`、`MAX_UPLOAD_BATCH_ENTRIES`、`MAX_ARCHIVE_BYTES`、`MAX_ARCHIVE_ENTRIES` 设置后台不可突破的绝对上限；并发、事务锁、密码验证队列和磁盘安全余量仍由服务固定保护。

多存储部署还需要按实际情况设置：

- `LOCAL_STORAGE_MOUNTS`：额外挂载点 JSON 数组，例如 `[{"id":"archive","name":"归档盘","path":"/srv/ycloud/archive"}]`。后台只能从这里声明的目录中选择，不能随意输入服务器路径。
- `S3_ALLOWED_ENDPOINTS`：MinIO、RustFS 和通用 S3 Endpoint 精确白名单，多个 Origin 用逗号分隔；阿里 OSS 与腾讯 COS 必须使用与 Region 对应的官方 HTTPS Endpoint。
- 阿里 OSS 不支持标准 S3 `PutObject If-Match/If-None-Match`。Ycloud 对阿里 OSS 使用 `x-oss-forbid-overwrite` 保护首次写入，并在进程内写锁下校验 ETag 后更新或删除。当前数据面按单写实例设计：同一 Bucket/Prefix 不得同时由多个 Ycloud 实例写入。
- 单机默认自动创建 `.ycloud-system/secrets/master.key`，用于加密 S3 SecretKey 和管理员 TOTP 密钥。正式 Linux 部署会把密钥文件限制为 `0600`；Windows 开发运行使用当前用户的 DPAPI 保护。必须随配置一起备份，丢失后加密配置无法恢复。
- Docker、集群和专业部署优先使用 `YCLOUD_CONFIG_KEY_FILE` 指向只读 Secret 文件；`YCLOUD_CONFIG_KEY` 环境变量保留为覆盖项。两者内容都是 32 个随机字节的 URL-safe Base64（无 `=`）。

PowerShell 生成配置主密钥：

```powershell
$keyBytes = [byte[]]::new(32)
[Security.Cryptography.RandomNumberGenerator]::Fill($keyBytes)
$configKey = [Convert]::ToBase64String($keyBytes).TrimEnd('=').Replace('+', '-').Replace('/', '_')
# 集群中把 $configKey 写入受保护的 Secret 文件，并设置 YCLOUD_CONFIG_KEY_FILE。
```

测试自建 RustFS 时还需例如：`$env:S3_ALLOWED_ENDPOINTS = "http://192.168.2.37:9000"`。测试连接会执行创建、读取、复制和删除临时对象，不只检查端口。

真实 S3 smoke 默认验证小对象完整生命周期。审核环境可设置 `YCLOUD_S3_SMOKE_MULTIPART=1`，额外执行跨过 64 MiB 阈值的流式 Multipart、逐段下载校验及大文件复制/移动/删除；设置 `YCLOUD_S3_SMOKE_CHECK_INVALID_CREDENTIALS=1`，验证服务拒绝正确 Access Key ID 与错误 Secret 的组合；设置 `YCLOUD_S3_SMOKE_INTERRUPTED_MULTIPART=1`，在首个分片提交后注入请求体断流，并检查目标对象不可见、用户容量为零且没有遗留 Multipart 会话。隔离 LAN 中的 HTTP Endpoint 还可设置 `YCLOUD_S3_SMOKE_TRANSPORT_CUT=1`，经回环故障代理在首个 UploadPart 传输中强制断开 TCP 连接，并验证后续 Abort 清理；设置 `YCLOUD_S3_SMOKE_SUSTAINED_OUTAGE=1` 会在断开 UploadPart 后保持代理离线，使即时 Abort 也失败，再恢复网络并验证持久化分片会话能够被恢复流程终止。两个网络故障开关都拒绝 HTTPS Endpoint，不能用于生产流量。这些测试只可使用隔离 Bucket/Prefix，凭据不得写入仓库或命令历史。

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

结构和安全边界见 [ARCHITECTURE.md](ARCHITECTURE.md)，安全审核范围与执行结果见 [SECURITY_AUDIT.md](SECURITY_AUDIT.md) 和 [SECURITY_AUDIT_REPORT.md](SECURITY_AUDIT_REPORT.md)。
