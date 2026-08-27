# Ycloud 架构

## 技术栈

- 后端：Rust 2021、Tokio、Axum 0.8。
- 前端：Vue 3.5、TypeScript、Vite；不使用 Pinia 和 Vue Router。
- 存储：本地文件系统与 S3 SigV4 兼容对象存储。

## 请求与存储链路

```text
浏览器 / WebDAV
  -> Axum 路由与安全中间件
  -> 后端认证和按 storage_id 授权
  -> StorageRegistry
  -> 本地事务存储 / S3 对象存储
```

- `app.rs`：路由、中间件、存活与就绪探针。
- `api.rs`、`admin_api.rs`：网页及管理 API。
- `auth.rs`、`login_security.rs`、`security.rs`：会话、密码、登录限制和代理边界。
- `directory_listing.rs`：统一目录排序、搜索和游标分页。
- `storage_backend.rs`：多存储注册与统一操作接口。
- `capacity.rs`：存储容量预留、持久账本和后台核对。
- `storage.rs`、`storage_transaction.rs`：本地路径边界、流式 I/O 和事务恢复。
- `s3_backend.rs`：S3 分页、对象操作和恢复事务。
- `webdav.rs`、`archive.rs`：WebDAV 与流式 ZIP。

Vue 页面直接使用正式 URL，由小型路由函数选择视图；共享状态留在组件或 composable，不额外引入全局状态库。

## 安全和数据边界

- 网页访客只有被允许存储的只读能力；管理员拥有后台和全部文件权限；普通账号由管理员按存储授权且不能进入后台；WebDAV 使用独立挂载凭据。
- 所有权限由后端验证，前端隐藏操作不作为安全边界。
- 存储实例以不可变 `storage_id` 隔离，路径不包含存储名称。
- 本地路径拒绝符号链接和 Windows 重解析点；写入、替换、复制和删除经过 `.ycloud-system` 事务区。
- S3 凭据加密保存且不回显；S3 移动执行“复制完成后删除源对象”，不宣称原子移动。
- 文件、下载和 ZIP 均流式处理；目录页只保留当前页候选，不把完整目录载入内存。
- 容量账本在写操作后持久化并定时核对；配置了容量上限且账本不可信时暂停写入。
- 会话和访问令牌同时受全局数量与单主体数量限制。

默认限制：上传 5 GiB；ZIP 3 GiB/1000 项；磁盘保留 512 MiB；本地阻塞 I/O 并发 4；归档任务并发 1。资源硬边界不允许通过后台任意放大。

## 部署边界

- 默认监听 `127.0.0.1:18473`。
- `/api/health` 只表示进程存活；`/api/ready` 检查默认存储可用性。
- 非回环监听必须选择可信局域网模式或可信 HTTPS 反向代理模式；代理头只信任 `TRUSTED_PROXY_IPS`。
- Ycloud 不内置 TLS。运行配置、日志、凭据、用户文件、`target/` 和 `node_modules/` 不进入 Git。
