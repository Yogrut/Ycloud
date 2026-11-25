# 身份与权限边界审查（2026-11-25，第一轮）

范围：`src/app.rs`、`src/security.rs`、`src/api.rs`、`src/file_access.rs`、
`src/webdav/access.rs`、`src/auth.rs`、`src/auth/session.rs`。

本轮结论与决定，供后续轮次引用。

## 路由分层（已核实）

- 公开：`/api/health`、`/ready`、`/logout`、`/me`、`/user/traffic`、`/login`、
  `/user/login`、`/gate`（`app.rs:44-56`）。
- 读受保护：storages/files/changes/directory-size/download/archive/preview/
  thumbnail/folder-unlock（`app.rs:66-79`）。
- 写受保护：delete/mkdir/upload*/rename/batch*（`app.rs:90-124`），gate 令牌
  在 `write_auth_middleware` 被拒（`auth.rs:588-598`）。
- 管理：`/api/admin/*` 叠 admin 认证、CSRF、4 并发变更闸（`app.rs:144-220`）。
- WebDAV 独立认证（`webdav/access.rs:17-88`）。

## 决定

| 编号 | 事项 | 决定 |
| --- | --- | --- |
| R1 | 锁检查使用未规范化路径 | **不改代码**。`path_is_same_or_descendant`（`config.rs:157`）与 `paths_overlap` 自行忽略空段和 `.`；`..` 由 `normalize_relative`（`storage/path.rs:132`）拒绝。新增测试固定该不变量 |
| R2 | `list_files` 先取 metadata 再查锁 | **不改代码**。被锁目录在父列表本就可见（`api.rs` 的 `is_locked` 标记），404/403 差异不泄漏新信息 |
| R4 | CSRF 比较 Origin 时未比较协议 | 低风险，暂不改。**生产部署请配置 `public_base_url`**，配置后按 `security.rs:292-299` 做完整比较 |
| R5 | WebDAV 缺连接地址时回落 127.0.0.1 | 已加注释说明（`webdav/access.rs:40`） |
| R6 | 仅浏览权的访客看不到缩略图 | 产品决定待定：缩略图/预览目前要求 `Download`（`api.rs:704-714`） |
| R7 | `/api/upload/status` 在写路由层 | 保持现状。访客无上传权限，不会创建任务，也就不会轮询该接口 |

## 新增测试

`src/api.rs::permission_matrix_tests`：

1. `anonymous_requests_are_rejected_before_any_handler_runs` —— 无 cookie 的读/写均为 401。
2. `gate_reads_but_cannot_write_or_enter_admin` —— gate 读 200、写 403、管理 401。
3. `read_only_user_reads_but_cannot_write` —— 只读用户读 200、写 403。
4. `folder_locks_match_equivalent_path_spellings_and_reject_traversal` ——
   `locked`、`/locked`、`//locked`、`./locked`、`locked//sub` 均 403；
   未锁定目录 200；`../outside` 400。

## 待补的运行时验证（部署机执行）

Q1–Q4 已在单元/路由层覆盖；上线后仍建议用 curl 复验一次，命令见审查报告
（随本轮对话提供）。Q5–Q8 未执行，保持"疑问"状态。
