# 上传与事务一致性审查（第三轮，进行中）

范围：`upload_batch.rs`、`upload_recovery.rs`、`storage_transaction.rs`、
`storage/atomic_write.rs`、`storage/cleanup.rs`、`direct_upload.rs`。
只按 Linux 语义审查；Windows/macOS 仅本地开发。

## 第 1 组：并发同名上传

### 策略事实

| 策略 | 入口 | 依据 |
| --- | --- | --- |
| 保留两者 | prepare 阶段自动编号 | `upload_batch.rs:1229-1273`、`numbered_upload_name` `:1129-1162` |
| 覆盖 | 浏览器/直传**没有**；只有 WebDAV PUT | `api.rs:647-655`、`storage_backend.rs:817-828`、`webdav.rs:303-318` |
| 跳过 | 服务端没有，纯客户端决定 | 前端上传弹窗无冲突选项 |

### create-only 的原子边界（证据）

- 快速失败检查（非安全边界）：`storage_backend.rs:920-925`、
  `storage_transaction.rs:805-809`。
- 真正边界：`storage/atomic_write/publication.rs:71` 取锁后，
  `storage_transaction/transfer.rs:100` 调用
  `rooted_rename_noreplace(temporary, destination)`，
  落到 `linux_root.rs:211,273` 的 `renameat2(..., RENAME_NOREPLACE)`。
- 覆盖路径：旧文件先 `rename_noreplace` 到 `backups/<journal id>`（`transfer.rs:97-98`），
  临时文件再无覆盖重命名到目标（`:100`），崩溃由 journal 恢复。
- 非 Linux 分支：`storage_transaction.rs:435-445` 是 `try_exists` + `rename`，
  存在 TOCTOU；记录，不处理（部署目标只有 Linux）。

### 并发覆盖靠什么串行化（补充证据）

- 每个存储实例持有一个 `mutation_gate: Arc<AsyncMutex<()>>`
  （`storage.rs:69,216,234`）。
- 每个写入者拿到同一个 `Arc` 的克隆（`storage/atomic_write.rs:102`）。
- 发布阶段在 `storage/atomic_write/publication.rs:71` 取锁，随后
  `transfer.rs:96-156` 完成"搬旧文件 → 放新文件 → 落 receipt → 清理备份"，
  全程持锁；因此两个并发 PUT 是**按存储串行**的，不是按路径。
- 串行化后，第二个请求看到目标已存在，走 replacing 分支，备份 id 来自
  独立 journal（随机 id），不会与第一个的备份冲突。

### 测试

| 测试 | 内容 |
| --- | --- |
| `upload_batch.rs::a_name_taken_after_planning_fails_the_commit_without_overwriting` | planning 后、提交前插入抢占文件；提交必须 Conflict，抢占字节保留 |
| `webdav.rs::concurrent_put_keeps_one_whole_body_and_if_none_match_lets_one_win` | 两个不同长度并发 PUT，最终内容恰好等于一方完整内容；`If-None-Match: *` 并发时恰好一个 201、一个 412 |
| 既有 | `concurrent_preparation_reserves_distinct_names...`（2678）、`twelve_users_uploading_the_same_name_keep_every_payload`（2871）、`concurrent_confirmation_starts_an_upload_only_once`（3168） |

### 稳定性

本地把两条新测试各循环 200 次（直接运行测试二进制，跳过 cargo 启动）：

```
a_name_taken_after_planning...                     200 runs, failures=0
concurrent_put_keeps_one_whole_body...             200 runs, failures=0
```

未出现"输的一方返回 409 而不是 412"的偶发情况。

## 第 2 组：取消与中断恢复（进行中）

### 已核实的设计

- 状态：`Pending` / 运行中 / `Complete` / `Failed` / `Unknown`
  （`upload_batch.rs:73,186,200,397-399`）。
- 恢复不猜结果：用 `operation_id(ticket,path)` + size 向后端求证，
  只有明确 `Committed`/`NotCommitted` 才落终态
  （`upload_recovery.rs:81-98`）。
- 恢复按后端分组、带预算、游标轮转，坏条目不会饿死其他条目
  （`upload_recovery.rs:49-99,104-117`）。

### 已有测试

`mixed_status_reservations_and_cancellation_keep_uncertain_results`（1976）、
`cancellation_keeps_published_state_when_directory_sync_is_unconfirmed`（2411）、
`interrupted_execution_retains_unknown_but_never_overwrites_a_completed_result`（2620）、
`upload_waiter_cancellation_still_registers_completion...`（2757）、
`cancellation_is_storage_bound_and_idempotent`（3117），以及
`upload_recovery.rs:243-982` 的 17 条恢复测试。

### 待做

`tests/upload_crash.rs`（`#[cfg(target_os = "linux")]`，串行）：

1. 一半请求体时 `kill -9` → 重启后状态为失败/不存在，`storage/.ycloud-system`
   无临时文件残留。
2. 上传完成后立即 `kill -9` → 重启后文件内容完整、状态成功。

不向生产代码加失败注入；"已写入未确认"继续由现有单测覆盖。
