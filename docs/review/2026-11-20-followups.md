# 待办（2026-11-20）

## 前端

1. 补 6 条上传队列组合测试：`blocksRetry`、`upload_batch_expired`、`commit=not_committed`、`commit=unknown`、队列边界 5a（MAX-1 接受）/5b（MAX 拒绝，`prepareUploadBatch` 未被调用）。
   - 1 和 3b：`getUploadBatchStatus` 设成永不 resolve，结尾调 `disposeUploads()`
   - 2a / 2b 各一条；3b 补断言 `safeToPrepare === false`
   - 5a/5b 用 `MAX_UPLOAD_QUEUE_TASKS` 构造边界对，不写死数字
2. `git mv frontend/src/apps/browser/uploadQueue.completion.test.ts frontend/src/apps/browser/useUploadQueue.completion.test.ts`
3. 把 `uploads()`、`settle()`、`ticketedTask()` 抽到共享测试辅助，`useUploadQueue.test.ts` 与新文件共用，避免两份夹具漂移。

## 后端 / 构建

4. Linux 上完整跑一遍：`cargo fmt --check`、`cargo clippy --all-targets --all-features --locked -- -D warnings`、`cargo test --all-targets --locked`。Windows 未编译 `linux_root.rs` 与 `recovery.rs` 的 Linux 专用分支。
5. 提交顺序：基线（冲突策略 + 选目录弹窗）→ B2 → B1（`git add -p` 只暂存变体统一）→ 其余；每个提交单独跑 `cargo test --lib`。
6. 前端构建后确认 `static/app` 与源码同步提交（CI 会检查）。

## 已确认但未修的取舍

- 守卫测试扫文本、不区分注释：接受，报错信息已指明改用 `AppError::DestinationExists`。
- 回滚恢复二次崩溃返回 `Conflict`，文案写明删除日志会触发备份清理：接受，已写进设计文档。
- keep_both 并发撞号：进程内靠 `mutation_gate` 与 `create_new` 保证不覆盖，外部写入者竞态仍可能覆盖，与"S3 替换非原子"同类取舍。
