# Ycloud 架构

## 技术栈

- 后端：Rust 2021、Tokio、Axum 0.8。
- 前端：Vue 3.5、TypeScript、Vite。
- 存储：本地文件系统、S3 SigV4 兼容对象存储。
- 部署：Linux、Docker、Docker Compose。

## 请求链路

```text
浏览器 / WebDAV
  -> Axum 路由与安全中间件
  -> 认证与 storage_id 授权
  -> StorageRegistry
  -> 本地存储 / S3 存储
```

## 后端模块

| 模块 | 职责 |
| --- | --- |
| `bootstrap.rs`、`state.rs` | 启动流程、共享运行状态和后台清理任务 |
| `app.rs` | 路由、中间件、存活和就绪接口 |
| `api.rs`、`batch_operations.rs`、`file_access.rs` | 单项／批量文件 API、授权和路径策略 |
| `admin_api.rs`、`admin_api/` | 管理 API、用户和 TOTP |
| `admin_execution.rs` | 有界管理操作执行，接收完整请求后不随 HTTP 等待者取消 |
| `auth.rs`、`login_security.rs`、`security.rs`、`totp.rs` | 认证、会话、TOTP、登录限制和代理边界 |
| `config.rs`、`config/` | 配置模型、迁移、校验、持久化和密钥 |
| `domain_binding.rs` | HTTPS 域名绑定和可信代理配置 |
| `storage_backend.rs`、`storage_catalog.rs` | 存储注册表、统一接口和部署挂载目录 |
| `storage.rs`、`storage/`、`storage_transaction.rs` | 本地路径、读写和事务恢复 |
| `s3_backend.rs`、`s3_backend/` | S3 客户端、对象操作、Multipart 和恢复 |
| `capacity.rs` | 容量预留和账本 |
| `upload_batch.rs` | 批量上传清单和上传票据 |
| `directory_listing.rs`、`directory_size.rs`、`directory_snapshot.rs` | 排序、搜索、分页、按需大小统计和有界目录快照 |
| `archive.rs` | 流式 ZIP |
| `traffic.rs`、`transfer_limit.rs` | 流量账本、额度统计和传输速率限制 |
| `webdav.rs`、`webdav_path.rs`、`webdav_xml.rs` | WebDAV 协议、路径处理和 XML 响应 |

## 前端结构

- `frontend/src/apps/`：登录、文件浏览、预览和管理页面；页面专用状态与操作逻辑和页面放在同一目录，例如 `apps/browser/useBrowser*.ts`。
- `frontend/src/apps/login/main.ts`：前端唯一启动入口，由 `App.vue` 根据路径选择页面。
- `frontend/src/components/ui/`：可直接维护的基础 UI 组件源码。
- `frontend/src/lib/`：基础 UI 组件使用的通用工具。
- `frontend/src/shared/api/`：API 客户端。
- `frontend/src/shared/components/`：共享组件。
- `frontend/src/shared/composables/`：共享状态逻辑。
- `frontend/src/shared/i18n/`：中英文资源。
- `frontend/src/shared/styles/`：主题和布局。
- `static/app/`：生产构建产物。

## 不变量

- 权限、路径、容量和上传票据由后端验证。
- 每个操作绑定不可变 `storage_id` 和规范化路径。
- 本地路径拒绝符号链接和 Windows 重解析点。
- 本地写入使用原子替换和事务恢复。
- S3 移动按“复制成功后删除源对象”执行，不保证原子性。
- S3 Multipart 会话持久化；失败后执行 Abort 或在存储恢复时清理。
- 同一 S3 Bucket/Prefix 只允许一个活动写实例。
- S3 SecretKey 和管理员 TOTP 密钥使用 AES-256-GCM 加密。
- 文件、下载、目录和 ZIP 使用有界内存或流式处理。
- 会话、令牌、密码验证、文件 I/O 和归档任务均限制并发。

## 配置与文件响应边界

- `storage/response.rs` 的 `FileResponsePolicy` 同时供本地和 S3 使用，WebDAV 文件读取也走这条路径。附件响应、预览类型、`nosniff` 和文件 CSP 不再分散决定；全局 CSP 追加而不覆盖文件 CSP。
- 配置先形成完整候选，经版本迁移、解密、反序列化和业务校验后再落盘。主配置语义或解密错误不会自动回退旧安全策略；主文件缺失或 JSON 语法错误时，备份也必须先通过完整校验。
- `config/commit.rs` 将同目录临时文件重命名到主文件作为发布点。发布后的目录同步或备份刷新失败返回已发布的 `ConfigCommit` 状态并告警，不能当成“完全没保存”再次提交。Unix 在创建时限制文件权限并同步目录；Windows 仅完成文件同步与重命名，未验证断电持久性。
- 迁移前将原始配置整体加密保存到单个 `config.json.pre-migration` 恢复槽（自定义配置路径沿用同目录扩展名规则）。它可能包含历史凭据，需与主密钥一同保护；下次迁移覆盖，不在普通保存时轮换。`config::read_migration_backup` 只读解密导出，不自动恢复旧策略；尚无恢复 CLI 和按时间清理机制。
- 管理写请求在认证与 CSRF 检查后完整接收（最多 64 KiB），再进入独立执行任务；最多 4 个并发操作。HTTP 等待结束后操作仍可能完成，超时应核对当前设置。通用配置更新任务自行持有更新锁，直到磁盘与内存发布完成。
- 以上不等于完整持久化任务系统：备份维护自动重试、操作结果查询、进程退出/崩溃恢复、配置 revision 与后端 readiness 分离仍属后续治理范围。普通 I/O 故障与请求取消测试不替代 Linux 容器及断电验证。

## 部署边界

- 默认监听 `127.0.0.1:18473`。
- Ycloud 不内置 TLS。
- 后台未绑定域名时使用 HTTP；绑定 HTTPS 域名后启用严格域名校验、Secure Cookie 和 HSTS。
- DNS、证书和转发属于部署环境；Ycloud 不要求代理 IP 或 `X-Forwarded-*` 参数。
- `LOCAL_STORAGE_MOUNTS` 限制后台可选本地路径。
- `S3_ALLOWED_ENDPOINTS` 限制自建 S3 Endpoint。
- `YCLOUD_CONFIG_KEY_FILE` 用于外部配置主密钥。
- 运行配置、日志、凭据、用户数据、`target/` 和 `node_modules/` 不进入 Git。

## 工程门禁

- Rust：fmt、Clippy、测试、release 构建、cargo-audit、cargo-deny。
- 前端：ESLint、TypeScript、Vitest、生产构建、npm audit。
- 工具链：Rust 1.96.1；最低兼容版本 1.94.1。
