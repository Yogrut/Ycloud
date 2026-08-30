# Ycloud 架构

## 技术栈

- 后端：Rust 2021、Tokio、Axum 0.8。
- 前端：Vue 3.5、TypeScript、Vite；不使用 Pinia 和 Vue Router。
- 存储：本地文件系统与 S3 SigV4 兼容对象存储。
- 生产部署目标：Linux 原生进程、Docker 和 Docker Compose。Windows 只用于开发编译与本机回归，不作为生产支持平台。

## 请求与存储链路

```text
浏览器 / WebDAV
  -> Axum 路由与安全中间件
  -> 后端认证和按 storage_id 授权
  -> StorageRegistry
  -> 本地事务存储 / S3 对象存储
```

- `app.rs`：路由、中间件、存活与就绪探针。
- `api.rs`、`admin_api.rs`：网页及管理 API；`admin_api/totp.rs` 与 `admin_api/users.rs` 分别封装管理员两步验证和普通用户账户生命周期。
- `auth.rs`、`login_security.rs`、`security.rs`：会话、密码、登录限制和代理边界。
- `config.rs`、`config/`：配置公共入口，以及持久化模型、校验、环境参数、schema 迁移、原子保存和密码处理模块。
- `directory_listing.rs`：统一目录排序、搜索和游标分页。
- `storage_backend.rs`：多存储注册与统一操作接口。
- `capacity.rs`：存储容量预留、持久账本和后台核对。
- `storage.rs`、`storage/`、`storage_transaction.rs`：本地存储入口，以及路径边界、流式响应、原子写入、磁盘预留和事务恢复模块。
- `s3_backend.rs`、`s3_backend/`：统一 S3 后端入口，以及客户端、键空间、目录分页、容量扫描、对象读取、响应体和目录事务模块。
- `webdav.rs`、`archive.rs`：WebDAV 与流式 ZIP。
- `upload_batch.rs`：批量上传清单校验、短期票据和单文件流式提交绑定。

Vue 页面直接使用正式 URL，由小型路由函数选择视图；共享状态留在组件或 composable，不额外引入全局状态库。浏览页把上传批次状态机放在 `useUploadQueue.ts`，每项任务冻结 `storage_id`、入队目录和目标路径，页面切换存储不会改变已经创建的任务。所有 JSON API 客户端共用错误信封解析，保留 HTTP 状态、稳定错误码和 `x-request-id`。

## 工程门禁

- Rust 最低版本为 1.94.1，仓库开发与 CI 工具链固定为 1.96.1。
- Linux 执行 Rustfmt、Clippy、全量测试和 release 构建，并作为发布门禁；Windows 只执行可移植性编译与功能回归，不承担生产部署安全验收。
- 前端执行 ESLint、TypeScript、Vitest、生产构建和 npm audit；构建后必须确认 `static/app` 与源码一致。
- Cargo audit 与 cargo-deny 检查漏洞、许可证、来源和依赖策略。例外必须同时写明不可达依据和移除条件。

## 安全和数据边界

- 网页访客只有被允许存储的只读能力；管理员拥有后台和全部文件权限；普通账号由管理员按存储授权且不能进入后台；WebDAV 使用独立挂载凭据。
- 所有权限由后端验证，前端隐藏操作不作为安全边界。
- 存储实例以不可变 `storage_id` 隔离，路径不包含存储名称。
- 本地路径拒绝符号链接和 Windows 重解析点；写入、替换、复制和删除经过 `.ycloud-system` 事务区。
- Ycloud 统一秘密层通过 AES-256-GCM 加密 S3 SecretKey 和管理员 TOTP 密钥；单机主密钥由系统保护并自动管理，集群使用外部 Secret 文件。S3 驱动不自行管理密钥。
- 管理员可启用兼容 2FAuth 的标准 TOTP；恢复码仅保存 Argon2 哈希且单次使用。
- S3 移动执行“复制完成后删除源对象”，不宣称原子移动。
- S3 大文件上传和超过单次复制安全范围的对象使用分片操作；首个分片前将 `key` 与 `upload_id` 写入受限内部恢复区，失败时先主动中止，网络不可用则保留记录并在存储重新激活时继续清理。S3 的 CreateMultipartUpload 与恢复记录写入无法组成原子事务，部署侧仍应配置“终止过期未完成 Multipart”生命周期规则作为最终兜底。
- 阿里 OSS 的 `PutObject` 不支持标准 S3 条件头；适配层使用 `x-oss-forbid-overwrite` 保护新建，并在共享进程内写锁下执行 ETag 前置校验后更新或删除。该保证只覆盖单个 Ycloud 进程，因此同一 Bucket/Prefix 当前只允许一个活动写实例。
- 第三方 S3 客户端只启用协议明确要求的 SDK 校验和，避免可选 `aws-chunked` checksum trailer 与厂商实现不兼容；Ycloud 仍校验请求长度、响应 ETag、对象长度和下载内容。
- 文件、下载和 ZIP 均流式处理；目录页只保留当前页候选，不把完整目录载入内存。
- 容量账本在写操作后持久化并定时核对；配置了容量上限且账本不可信时暂停写入。
- 会话和访问令牌同时受全局数量与单主体数量限制。

默认业务限制：单文件上传 5 GiB，单批次上传 20 GiB/1000 个文件，ZIP 3 GiB/1000 项。管理员可以在部署环境给出的绝对上限内调整这些值；文件夹上传保留相对路径，空目录不作为文件上传。

固定资源保护：磁盘保留 512 MiB，本地阻塞 I/O 并发 4，归档任务并发 1，密码验证和 WebDAV 均有有界队列。事务锁、并发数、活动上传票据数和磁盘余量不开放给后台任意放大。

## 部署边界

- 正式部署和安全发布结论只覆盖 Linux 原生、Docker 与 Docker Compose；Windows 服务安装、ACL/DACL 和生产运维不在支持范围内。
- 默认监听 `127.0.0.1:18473`。
- `/api/health` 只表示进程存活；`/api/ready` 检查默认存储可用性。
- 非回环监听必须选择可信局域网模式或可信 HTTPS 反向代理模式；代理头只信任 `TRUSTED_PROXY_IPS`。
- 回环/LAN 请求先校验 Host；自定义内网 Authority 必须进入 `ALLOWED_HOSTS` 精确列表。配置公网代理参数时，即使后端只监听回环地址也进入严格代理模式。
- `MAX_UPLOAD_BYTES`、`MAX_UPLOAD_BATCH_BYTES`、`MAX_UPLOAD_BATCH_ENTRIES`、`MAX_ARCHIVE_BYTES` 和 `MAX_ARCHIVE_ENTRIES` 构成管理员配置不可突破的部署上限。
- `LOCAL_STORAGE_MOUNTS` 声明后台可选本地边界，`S3_ALLOWED_ENDPOINTS` 声明自建 S3 目标；`YCLOUD_CONFIG_KEY_FILE` 可覆盖单机自动主密钥以支持容器和集群。
- Ycloud 不内置 TLS。运行配置、日志、凭据、用户文件、`target/` 和 `node_modules/` 不进入 Git。
