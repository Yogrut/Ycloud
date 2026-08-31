# Ycloud

Rust + Vue 构建的自托管文件管理服务，支持本地存储和 S3 兼容对象存储。

## 支持范围

- 生产部署：Linux 原生进程、Docker、Docker Compose。
- Windows：开发、编译和本机运行。

## Windows 开发运行

```powershell
cd Ycloud-v2
cargo run --release --locked
```

访问 `http://127.0.0.1:18473`。首次启动凭据位于 `initial-credentials.json`。

## 功能

- 本地存储、阿里云 OSS、腾讯云 COS、MinIO、RustFS 和通用 S3。
- 上传、下载、预览、搜索、移动、复制、重命名、删除和 ZIP 归档。
- 管理员、访客、普通用户和 WebDAV 独立授权。
- 文件夹锁、TOTP、登录限制、访问日志和传输限制。
- 流式传输、本地原子写入、事务恢复和 S3 Multipart。

## Docker Compose

```bash
mkdir -p data
sudo chown 10001:10001 data
sudo chmod 700 data
docker compose up -d --build
docker compose ps
```

访问 `http://服务器IP:18473`。读取首次启动凭据：

```bash
sudo cat ./data/initial-credentials.json
```

修改管理员密码和网页访问密码后，该文件自动删除。`./data` 包含配置、主密钥、本地文件和运行状态，备份时整体处理。

## Linux 原生运行

```bash
cargo build --release --locked
BIND_ADDRESS=0.0.0.0 ALLOW_LAN_HTTP=true ./target/release/ycloud
```

默认配置和存储目录为当前目录下的 `config.json`、`storage/`。

## 部署配置

| 变量 | 用途 |
| --- | --- |
| `BIND_ADDRESS` | 监听地址；原生运行默认 `127.0.0.1` |
| `PORT` | HTTP 端口，默认 `18473` |
| `CONFIG_PATH` | 配置文件路径 |
| `STORAGE_PATH` | 默认本地存储路径 |
| `ALLOW_LAN_HTTP` | 允许非回环地址直接使用 HTTP |
| `ALLOWED_HOSTS` | 允许的内网 Host，多个值用逗号分隔 |
| `LOCAL_STORAGE_MOUNTS` | 后台可选本地挂载点的 JSON 数组 |
| `S3_ALLOWED_ENDPOINTS` | 自建 S3 Endpoint 白名单，多个值用逗号分隔 |
| `RUST_LOG` | 日志级别 |

Dockerfile 已设置容器内的监听地址、端口和数据路径，普通 Compose 部署只需 `ALLOW_LAN_HTTP=true`。

### HTTPS 反向代理

Ycloud 不内置 TLS。反向代理模式需要同时设置：

```env
ALLOW_LAN_HTTP=false
SECURE_COOKIES=true
PUBLIC_BASE_URL=https://cloud.example.com
TRUSTED_PROXY_IPS=172.18.0.1
```

代理必须传递原始 `Host`、单值 `X-Forwarded-For` 和 `X-Forwarded-Proto: https`。

### 配置主密钥

单机自动生成主密钥并保存在数据目录。Docker、集群或外部密钥管理场景可设置 `YCLOUD_CONFIG_KEY_FILE`，也可用 `YCLOUD_CONFIG_KEY` 覆盖。密钥为 32 个随机字节的 URL-safe Base64；密钥丢失后，已加密的 S3 SecretKey 和 TOTP 密钥无法恢复。

### 存储

额外挂载点示例：

```env
LOCAL_STORAGE_MOUNTS=[{"id":"archive","name":"Archive","path":"/srv/ycloud/archive"}]
```

自建 S3 示例：

```env
S3_ALLOWED_ENDPOINTS=http://192.168.2.37:9000
```

阿里云 OSS 和腾讯云 COS 使用与 Region 对应的官方 HTTPS Endpoint。同一 Bucket/Prefix 只允许一个 Ycloud 实例写入。

### 传输上限

后台可调整业务上限；以下变量定义后台不可突破的部署上限：

- `MAX_UPLOAD_BYTES`
- `MAX_UPLOAD_BATCH_BYTES`
- `MAX_UPLOAD_BATCH_ENTRIES`
- `MAX_ARCHIVE_BYTES`
- `MAX_ARCHIVE_ENTRIES`

## 检查

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked

cd frontend
npm ci
npm run check
```

前端构建产物写入 `static/app` 并嵌入 Rust 可执行文件。

项目结构见 [ARCHITECTURE.md](ARCHITECTURE.md)，开发规则见 [CONTRIBUTING.md](CONTRIBUTING.md)。
