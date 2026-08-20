# Ycloud

Ycloud 是一个使用 Rust、Axum 和原生 HTML/CSS/JavaScript 构建的轻量级私有文件服务，将网页文件管理、目录访问控制和 WebDAV 挂载整合在单个可执行程序中。

项目长期按照以下权重维护：

> 安全 40%、性能 30%、稳定 30%；数据安全性与数据完整性并列优先。

Ycloud 面向个人设备、家庭服务器和小规模私有部署，不以企业网盘的功能数量为目标。

## 功能

- 响应式文件浏览器，支持明暗主题、当前目录搜索和移动端选择面板。
- 上传、新建目录、下载、打包下载、重命名、移动、复制和永久删除。
- 图片、音视频、PDF、文本和 Markdown 等常见格式预览。
- 独立的管理员登录、网页访问密码和网页文件夹锁。
- 多个 WebDAV 挂载，可分别设置路径、账号、密码和只读权限。
- 大文件流式上传、下载和 ZIP 打包，不把完整文件载入内存。
- 原子上传、文件操作事务、配置备份和启动恢复。
- 持久化登录失败限制和 IP 记录，管理员可在后台解除限制。
- 生产静态资源编译进程序，运行时不需要 Node.js 或外部 CDN。

网页访问密码只授予浏览、预览和下载权限；写入操作必须登录管理员。WebDAV 使用挂载自己的凭据，并且不能与网页文件夹锁保护的路径重叠。

## 快速开始

需要稳定版 Rust 工具链：

```bash
git clone https://github.com/YogruTi/Ycloud.git
cd Ycloud
cargo run --release --locked
```

默认监听 `127.0.0.1:18473`，文件存储在 `./storage`，配置写入 `./config.json`。

首次启动会生成相互独立的管理员密码和首页访问密码，并写入配置目录下的 `initial-credentials.json`。日志只显示文件位置，不输出密码；修改两项初始密码后，该明文凭据文件会被自动删除。

打开：

```text
http://127.0.0.1:18473
```

停止服务使用 `Ctrl+C`。

## 部署

局域网 HTTP 直连：

```powershell
$env:BIND_ADDRESS = "0.0.0.0"
$env:ALLOW_LAN_HTTP = "true"
$env:SECURE_COOKIES = "false"
cargo run --release --locked
```

局域网设备通过 `http://服务器局域网IP:18473` 访问。不要将这种模式直接暴露到公网。

公网部署必须使用同机 HTTPS 反向代理，并设置：

```text
SECURE_COOKIES=true
PUBLIC_BASE_URL=https://cloud.example.com
TRUSTED_PROXY_IPS=127.0.0.1
```

代理必须覆盖为单一 `X-Forwarded-For`，并发送 `X-Forwarded-Proto: https`。Ycloud 会拒绝不可信代理、错误 Host 和不完整的公网安全配置。

常用环境变量：

| 变量 | 默认值 | 说明 |
|---|---:|---|
| `BIND_ADDRESS` | `127.0.0.1` | 监听地址 |
| `PORT` | `18473` | HTTP 端口 |
| `STORAGE_PATH` | `./storage` | 文件目录 |
| `CONFIG_PATH` | `./config.json` | 配置文件 |
| `MAX_UPLOAD_BYTES` | `100 GiB` | 后台上传设置的部署级硬上限 |
| `DISK_RESERVE_BYTES` | `512 MiB` | 文件提交后保留的磁盘空间 |

默认单文件上传上限为 5 GiB；普通单文件下载不限制大小；打包下载默认最多包含 3 GiB 文件内容和 1,000 个文件与文件夹条目。这三个业务限制可在管理后台的安全边界内调整。

## WebDAV

在管理后台创建并启用挂载后，客户端地址为：

```text
https://cloud.example.com/dav/挂载名称/
```

支持 `OPTIONS`、`PROPFIND`、`GET`、`HEAD`、`PUT`、`DELETE`、`MKCOL`、`MOVE` 和 `COPY`。当前不支持 Class 2 的 `LOCK`、`UNLOCK` 与 `PROPPATCH`；依赖完整锁协议的客户端可能不兼容。

## 适用范围

Ycloud 适合单实例、少量明确权限边界和个人文件管理。目前不提供多租户、RBAC、回收站、文件版本、全文检索、断点续传、集群协调或 WebDAV Class 2。

运行配置使用 JSON 文件，会话和解锁令牌保存在内存中，服务重启后需要重新登录。永久删除不承诺物理安全擦除。公网使用者需要自行维护 HTTPS 反向代理和可靠备份。

## 项目结构

```text
Ycloud/
├── src/                 # Axum 路由、认证、存储事务、API 与 WebDAV
├── static/              # 内嵌页面、CSS 和原生 ES Modules
├── frontend/            # 分阶段替换中的 Vue 3.5 + TypeScript 候选前端
├── storage/             # 默认运行时文件目录，不提交内容
├── config.example.json  # 配置结构示例
└── ARCHITECTURE.md      # 当前版本的完整设计与安全边界
```

详细模块职责、认证模型、路径规则、事务恢复和资源限制见 [ARCHITECTURE.md](ARCHITECTURE.md)。待实施且已经确认的功能见 [PLANNED_FEATURES.md](PLANNED_FEATURES.md)。

## 开发检查

```bash
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo audit
cargo deny --locked check
cargo build --release --locked
```

Vue 候选前端当前只迁移首页登录，旧前端仍是生产入口。并行开发时保持 Rust 服务运行，再执行：

```bash
cd frontend
npm ci
npm run check
npm run dev
```

访问 `http://127.0.0.1:5173/v2/` 预览候选前端；依赖更新规则见 [frontend/README.md](frontend/README.md)。

运行时配置、初始凭据、安全日志和 `storage` 内容不得提交到 Git。安全问题请勿在公开 Issue 中附带真实密码、配置或私人目录截图。

## 许可证

仓库目前尚未提供 `LICENSE`。在许可证确定前，默认版权规则仍然适用；正式开放贡献或分发前应先添加明确许可证。第三方声明见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
