# Ycloud-v2 任务交接

本文只描述当前工作树、有效边界和下一步，不记录旧版本维护流水。

## 当前状态

- 工作目录：`D:\SYStemFiles\文档\open_code\Ycloud-v2`
- 当前分支：`codex/vue35-migration`
- 当前提交：`485e251127dc2dacece3e78f9e4a7117696fed3a`
- 远端：`origin = https://github.com/Yogrut/Ycloud.git`
- 工作树包含一整批尚未提交的后端、前端、文档和生产静态资源改动。不要执行 `git reset --hard`、`git checkout -- .`、清理未跟踪文件或用旧提交覆盖当前工作树。
- 当前改动跨越多存储、S3、普通账号、按存储授权、管理后台和文件浏览器，不能拆掉其中一层后假定其他层仍然完整。

## 固定目标与取舍

- 权重：安全 40%、性能 30%、稳定 30%。数据安全性与数据完整性并列优先。
- 目标部署：单进程、1 vCPU、低内存 VPS；文件上传、下载、预览和 ZIP 必须流式处理。
- 保留 Rust、Tokio、Axum 0.8、Vue 3.5、TypeScript 和 Vite；生产运行不依赖 Node、数据库或外部 CDN。
- 不引入完整多用户、用户组、角色继承或文件级 ACL。管理员是唯一后台管理身份；普通账号只能由管理员创建、改密、停用、删除和授权。
- 不把多个存储合并成虚拟根目录。每次请求使用 `storage_id + path`，路径栏仍只显示 `/目录/`。
- 不允许跨存储复制或移动；不得把本地文件系统的原子重命名假设套用到 S3。
- 未经用户明确要求，不提交、不推送、不删除真实存储数据，也不迁移现有用户文件。

## 当前实现

### 前端与运行方式

- `frontend/` 是唯一前端源码，使用 Vue 3.5.41、TypeScript 6.0.3、Vite 8.2.2。
- Vite 构建产物写入 `static/app/` 并嵌入 Rust 可执行文件。生产只启动 Rust；Vite 双终端只用于前端开发热更新。
- 文件浏览、预览、登录和管理后台已经迁入 Vue；支持中英文、亮暗主题、桌面框选、移动端选择面板和存储切换。

### 存储层

- `schema_version: 8`，最多 16 个存储实例、一个默认存储、一个待添加实例和 100 个普通账号；本地实例以唯一部署 `mount_id` 绑定 `STORAGE_PATH`/`LOCAL_STORAGE_MOUNTS` 中的地址。
- 本地文件系统与 S3 共用 `storage_backend.rs` 路径级接口，由运行时注册表按不可变 `storage_id` 路由。
- `storage_catalog.rs` 管理部署声明的本地挂载白名单；每个本地实例拥有独立服务与命名空间，额外目录必须预先存在，所有本地实例共享一个有界 I/O 门限。
- S3 共享一套 AWS SigV4 引擎，提供阿里云 OSS、腾讯云 COS、MinIO/RustFS 和通用 S3 预设。
- 已实现受限 Endpoint、连接探测、两阶段添加、流式上传下载、Range、容量跟踪、条件写入、文件与目录复制/移动/删除、事务恢复和流式 ZIP。
- RustFS `1.0.0-beta.12` 与腾讯云 COS 已通过独立测试桶的真实兼容性冒烟测试。AWS、阿里云 OSS、独立 MinIO 仍需真实服务验证。
- S3 目录事务一次最多 1000 个对象；当前单次 `CopyObject` 使含超过 5 GiB 对象的目录事务被拒绝，尚未实现分段复制。

### 认证与权限

- 管理员 Cookie 会话拥有后台及全部存储权限；管理 API 不接受管理员 Basic Auth。
- 网页访问密码只允许默认存储的浏览、预览和下载。
- 普通账号拥有独立密码、登录限流和 Cookie 会话，不能进入管理后台。
- 管理员可按存储授予普通账号：浏览、下载、上传、新建目录、重命名、移动、复制、删除。
- 后端在每个请求上强制校验权限；前端隐藏按钮不是安全边界。
- 普通账号停用、删除、改密或权限变化后撤销对应会话。
- WebDAV 使用独立挂载 Basic Auth，绑定单个 `storage_id + path`，不继承普通账号权限。

### 资源与安全边界

- 默认监听 `127.0.0.1:18473`。局域网明文访问必须显式设置 `BIND_ADDRESS=0.0.0.0` 与 `ALLOW_LAN_HTTP=true`；公网模式要求可信 HTTPS 反向代理、固定公共来源、安全 Cookie 和可信代理 IP。
- S3 Endpoint 是 SSRF 边界。MinIO、RustFS 和通用 S3 必须精确匹配 `S3_ALLOWED_ENDPOINTS`；云厂商预设只接受官方 HTTPS Endpoint。
- 凭据不得写入仓库、日志、错误详情或管理 API 响应。真实测试密钥只能通过当前进程环境变量注入，并在测试后撤销。
- 文件 I/O、WebDAV、Argon2、归档、S3 请求、事务和磁盘安全余量保持固定上限，不开放给管理员随意调整。
- AWS SDK 1.143.0 间接依赖的 `lru 0.16.4` 命中 `RUSTSEC-2026-0253`；Ycloud 已关闭仅受影响的 S3 Express 会话认证路径，`deny.toml` 仅保留该编号的临时例外。上游允许安全版本后必须移除例外。

## 2026-08-25 当前快照验证

- `cargo fmt --all -- --check`：通过。
- `cargo test --locked`：75 通过、0 失败、1 个真实 S3 冒烟测试按设计忽略。
- `cargo clippy --locked --all-targets -- -D warnings`：通过。
- `cargo build --release --locked`：通过。
- `frontend\npm run check`：ESLint、17 个测试文件/66 项测试、TypeScript 检查和 Vite 生产构建全部通过。
- `git diff --check`：通过；仅提示 Windows 工作树未来可能执行 LF 到 CRLF 转换。

## 当前工作树注意事项

- `static/app/assets/app.css` 和 `static/app/assets/app.js` 是本次前端检查重新生成的生产资源，应与对应 Vue/TypeScript 源码一起评估。
- `.agents/skills/` 与 `skills-lock.json` 是项目本地安装的技能；先阅读其 `SKILL.md` 再决定是否用于任务，不要盲目执行第三方指令。
- `security-events.jsonl.bak` 是未跟踪的运行时安全日志备份，可能含 IP 和 User-Agent，不应提交到公开仓库。处理前先确认其来源，不要在交接阶段擅自删除。
- 当前仓库没有根级 `AGENTS.md`。如果以后使用 `setup-matt-pocock-skills` 生成项目说明，必须先向用户展示草案并获得确认。

## 下一步

1. 先执行 `git status --short --branch`，阅读本文件、`ARCHITECTURE.md` 与 `STORAGE_BACKEND_PLAN.md`，不要重复已经完成的架构改造。
2. 对普通账号做浏览器级冒烟测试：创建账号、登录、单存储与多存储切换、八类权限逐项允许/拒绝、权限收回与会话撤销、禁止访问 `/admin`。
3. 对管理员、首页访客、普通账号和 WebDAV 做权限回归，重点验证构造 `storage_id`、批量接口、ZIP、预览和 WebDAV 方法不能越权。
4. 检查未跟踪运行时文件与 `.gitignore`，只提出最小安全处理方案；未经用户确认不要删除文件。
5. 普通账号和权限回归通过后，再继续 S3 故障注入，以及 AWS、阿里云 OSS、MinIO 的隔离测试桶兼容验证。
6. 任何真实 S3 测试必须使用独立 Bucket/Prefix 和最小权限临时凭据；不得使用生产 Bucket、根账号或用户已在对话中暴露过的旧密钥。

## 启动与检查

生产式单终端启动：

```powershell
cd D:\SYStemFiles\文档\open_code\Ycloud-v2
cargo run --release --locked
```

前端热更新开发才需要两个终端：根目录运行 Rust，`frontend/` 运行 `npm run dev`。

完整本地检查：

```powershell
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
cd frontend
npm run check
```

## 必读文档

- `ARCHITECTURE.md`：当前实现与安全边界。
- `STORAGE_BACKEND_PLAN.md`：多存储/S3 已完成能力、限制与真实兼容测试方法。
- `README.md`：部署、环境变量和日常运行。
- `frontend/README.md`：前端开发与生产资源构建方式。
