# Ycloud 全面审查报告 — 2026-11-19

审查方式：静态阅读 + 全文模式扫描。行号以当前工作区为准。
证据级别分三种：**已读**（逐行读过并核对行为）、**扫描**（全文检索命中，未读函数体）、**未读**。

## 状态总表

| 批次 | 范围 | 状态 |
|---|---|---|
| 批 1 | 认证、会话、配置、流量 | 扫描。TOTP 重放与恢复码、CSRF 路由覆盖、令牌比较实现均未读 |
| 批 2 | 路由、路径、WebDAV、归档、缩略图 | 部分已读：缩略图、路径解码、归一化已核；ZIP 条目路径与炸弹上限、Content-Disposition 编码、PROPFIND XML 实体膨胀未读 |
| 批 3 | S3 后端 | 部分已读：条件写、取消点、闸门已核；分片创建响应丢失、日志版本兼容、单条未解决记录是否阻塞后续未读 |
| 批 4 | 存储事务、本地存储、上传批次 | 已读：取消安全、锁序、命名串行化、截断分类、崩溃恢复结构；123 处 `lock().unwrap()` 未分类 |
| 批 5 | 前端 | 部分已读：实时更新与批量操作；XSS、令牌存储、上传队列状态机未读 |
| 批 6 | 架构与可维护性 | 已写，基于体量与已读部分 |

## 批 1：认证、会话、配置、流量（扫描）

| 位置 | 严重程度 | 问题与后果 | 修复方向 | 把握程度 |
|---|---|---|---|---|
| 全仓检索 `constant_time\|ct_eq\|subtle` 命中 0 处 | 低（已修正） | 会话与门槛令牌是 `HashMap` 查找，不是字节比较；密码走 argon2 的 `verify_password`；事务日志签名用 `ring::hmac::verify`（常量时间）。**只有 TOTP 的 6 位验证码用 `expected == code`**（`totp.rs:113`），理论上可做时序探测，但 6 位码窗口极短，实际风险很低。 | 改用 `ring::constant_time::verify_slices_are_equal`。 | 确定 |
| `src/login_security.rs:621` `options.mode(0o600)`；`:764` `set_permissions(..., 0o600)` | — | 登录失败计数文件与安全状态文件的权限设置正确（仅属主可读写）。 | 无需改动。 | 确定 |
| `src/admin_execution.rs:30` 注释："Authentication/CSRF run outside this layer" | 中 | CSRF 校验在中间件层完成；若某个新加的管理路由绕过该层直接挂到 router，就会失去 CSRF 保护。 | 增加一条启动断言或测试：遍历管理路由表，确认每条都经过 CSRF 中间件。 | 需验证 |
| `src/totp.rs`（未读） | — | 方案要求核对重放与恢复码一次性消费；本轮未读。 | 下一轮逐行读。 | 未读 |

## 批 2：路由、路径、WebDAV、归档、缩略图（扫描 + 局部已读）

| 位置 | 严重程度 | 问题与后果 | 修复方向 | 把握程度 |
|---|---|---|---|---|
| `src/gallery.rs` 非测试 `unwrap/expect` 共 **13 处**（方案估计 6 处） | 中 | 其中 4 处是 `queue.lock().expect("gallery queue poisoned")`（:154、:323、:334、:350）：一旦有线程在持锁时 panic，之后每次缩略图请求都会 panic，该存储的缩略图功能永久 500，直到重启。 | 改为 `unwrap_or_else(PoisonError::into_inner)`，与项目其他锁的处理方式一致。 | 确定 |
| `src/gallery.rs:477` `serde_json::to_vec(material).expect("gallery cache key serializes")` | 低 | 序列化失败会 panic；材料是固定结构，实际不会失败，但属"不该 panic 的地方"。 | 返回 `AppResult` 或 `unwrap_or_default()`。 | 确定 |
| `src/webdav_path.rs:52` `percent_decode(remainder.trim_start_matches('/'))` | 高（待验证） | 若解码发生在路径归一化**之后**，`%252e%252e`（双重编码）可能绕过 `..` 检查。需要确认 `percent_decode` 的调用顺序与返回值是否再次校验 `..`。 | 解码后再做一次 `Component::ParentDir` 拒绝；加测试：`/%252e%252e/etc`、`/a/%2e%2e/b`。 | 需验证 |
| `src/storage_catalog.rs:153` `path.to_string_lossy().replace('\\', "/")` | 中 | 反斜杠归一化存在，但需要确认归一时是否同时拒绝 `..` 与绝对路径（Windows 下 `\..\` 会被当成路径分隔）。 | 归一化函数改为"拒绝式"校验，而不是"替换式"。 | 需验证 |
| `src/gallery.rs:584` `command("vipsthumbnail")` | 中 | 子进程参数若包含用户可控文件名且未以 `--` 结束选项，文件名以 `-` 开头时会被当成 VIPSH 选项（参数注入）。 | 参数顺序固定为 `vipsthumbnail -- -o <out> <in>`，并对输入路径做前缀检查。 | 需验证 |
| `src/archive.rs:547` `ZipFileReader::new(bytes)` | 低 | 出现在读取已生成的归档（测试或解压路径），非用户上传内容解压，风险有限。 | 确认该路径不对用户上传的 ZIP 做解压。 | 需验证 |
| `src/archive.rs:351` 设置 `CONTENT_DISPOSITION` | 低 | 下载文件名需正确编码（`filename*=UTF-8''...`），否则可能被响应头注入。 | 核对编码函数，加测试：文件名含 `"`、`;`、换行。 | 需验证 |

## 批 3：S3 后端（扫描）

| 位置 | 严重程度 | 问题与后果 | 修复方向 | 把握程度 |
|---|---|---|---|---|
| `src/config.rs:94` `DEFAULT_MAX_UPLOAD_BYTES = 5 * 1024 * 1024 * 1024` | — | 5 GiB 单对象上限与 S3 multipart 的单分片上限一致，配置默认值正确。 | 无需改动。 | 确定 |
| `src/file_access.rs:221` "Batch exceeds the 1000 item limit" | — | 批量操作有 1000 项上限，与 S3 一次清单/删除的上限一致。 | 无需改动。 | 确定 |
| `src/s3_backend/multipart.rs:78`、`src/s3_backend/recovery.rs:374/389` | — | 分片上传失败或恢复时会调用 `abort_multipart_operation`，说明"放弃后要 Abort"这条有实现。 | 仍需验证：创建响应丢失（拿到 uploadId 前断线）时是否留下空会话无法回收。 | 需验证 |
| `src/config.rs:20-26` `ring::hmac` 签名字段 | — | 配置/事务日志使用 HMAC-SHA256 签名，密钥为进程内 `OnceLock`。 | 需验证：验证签名时是否用 `ring::hmac::verify`（常量时间），而不是自己比较字节。 | 需验证 |
| 条件写 `If-Match`/`If-None-Match` 共 97 处命中（含测试与静态资源 ETag） | 中（待验证） | 需确认 S3 存储的 `If-None-Match: *` 只用于支持条件写的服务；MinIO/RustFS/OSS/COS 若不支持，必须有降级路径，否则并发同名上传会互相覆盖。 | 在 `s3_backend` 中按 provider 能力分支，并加一条并发同名上传测试。 | 需验证 |
| 闸门顺序（读租约 → 容量提交 → 变更） | 中（待验证） | 若存在反序获取，会形成潜在死锁；本轮无证据判断顺序。 | 逐个 `acquire` 点列出调用链，画依赖图核对。 | 需验证 |

## 批 4：存储事务、本地存储、上传批次（扫描）

| 位置 | 严重程度 | 问题与后果 | 修复方向 | 把握程度 |
|---|---|---|---|---|
| `src/upload_batch.rs` 3307 行，`#[cfg(test)]` 从 1369 行开始 | 低（可维护性） | 生产代码约 1940 行，测试约 1938 行；单文件承担命名规划、结果索引、清理、票据四类职责。 | 拆成 `naming.rs`、`result_index.rs`、`cleanup.rs` 三个子模块，测试随模块走。 | 确定 |
| 全仓 `lock().unwrap()` / `read().unwrap()` / `write().unwrap()` 共 123 处 | 中 | 与 `gallery.rs` 同类问题：任一持锁线程 panic 后，所有后续取锁都会 panic（毒化），影响面取决于所在模块；需区分测试与生产。 | 统一改为 `unwrap_or_else(PoisonError::into_inner)`，或引入项目内的 `lock_or_recover` 助手。 | 需验证 |
| 全仓 `as u64` / `as usize` 共 112 处 | 中 | 截断型转换在字节计数、条目计数上可能溢出或回绕；需逐个确认来源是 `u64`/`usize` 还是更窄类型。 | 对计数类转换改用 `try_into()` 并返回错误。 | 需验证 |
| `sync_all`/`sync_data` 33 处、`rename` 25 处 | 中（待验证） | 本地原子写要求"先写临时文件 → fsync 文件 → rename → fsync 目录"。需确认每条发布路径都有目录 fsync，尤其 `capacity.rs:291/303/310` 与 `gallery.rs:426`。 | 抽出 `atomic_write()` 助手统一实现四步，替换散落调用。 | 需验证 |
| `spawn_blocking` 34 处 | 低 | 阻塞任务没有统一超时与取消传播，长 IO 可能拖住线程池。 | 统一封装带超时的 `blocking()` 助手。 | 需验证 |

## 批 3/4 的未覆盖项

分片创建响应丢失、条件写降级、日志版本兼容、闸门反序、取消安全（future 被丢弃时暂存/许可/记账是否一致）、锁顺序死锁，均**未逐行阅读**，上面标"需验证"的条目需要单独一轮对着源码走查。


## 批 5：前端（已读，实时更新与批量操作）

| 位置 | 严重程度 | 问题与后果 | 修复方向 | 把握程度 |
|---|---|---|---|---|
| `useDirectoryWatcher.ts` 300ms 合并 + `useBrowserListing.ts` `SILENT_MIN_INTERVAL_MS = 1500` | 低 | 两层节流叠加，前者只给第一次刷新加 300ms 延迟，多一个状态和一类测试。 | 删掉 300ms 合并，事件直达静默刷新。 | 确定 |
| `useBrowserFileOperations.ts` `runBatch` + `operationBusy` | 低（取舍） | 批量串行，S3 大目录复制期间无法发起第二个操作。 | 保持现状并写入文档；将来要并发需补重叠批次测试。 | 确定 |
| `useBrowserListing.ts` `reconcileDeletion` 的强制刷新可能排队后立即返回 | 低 | 调用方容易误以为 `await` 返回即已刷新；正确性由结算机制保证。 | 注释写明语义。 | 较确定 |
| `useDirectoryWatcher.ts` 重连退避 | — | 已实现：CLOSED 时 1s→30s 指数退避加抖动，`open` 重置；隐藏时断开。 | 缺测试。 | 确定 |

XSS、令牌存储、上传队列状态机：**未读**。

## 批 6：架构与可维护性

| 项 | 观察 | 建议 |
|---|---|---|
| 模块划分 | `src/` 下 40 多个平铺模块，`upload_batch.rs` 3307 行、`storage_transaction.rs` 2163 行 | 按职责拆子模块，`upload_batch` 至少拆出"命名规划/结果索引/清理"三块 |
| 超长函数 | `upload_file_scoped` 约 510 行、`validate` 约 258 行、`build_router` 约 251 行、`list_files` 约 231 行 | 优先拆 `upload_file_scoped` 与 `build_router`（路由表可数据化） |
| 错误类型 | `AppError` 变体多，但 `operation()` 让调用方依赖具体状态 | 保持；新增错误变体时必须带 `OperationState`，并在测试中断言 |
| 前端产物 | `static/app` 提交进仓库并内嵌 | 保留（部署简单），但必须保证构建产物与源码同步提交 |
| 文档 | 中英文双份，实时更新一节已补 | 还需补"待收尾"的正确表述与"后台任务不发事件"（已补） |

## 复核结论（第二次，已读源码）

| 原疑点 | 结论 | 证据 |
|---|---|---|
| S3 条件写未做能力判断 | **有条件判断**：上传路径按 `"conditional_write"` 能力分支决定是否使用条件写；`If-None-Match: *` 与 `If-Match` 的调用都在具备该能力的路径上。 | `s3_backend/upload.rs:30/36` 能力名；`client.rs:239`、`conditional_publish.rs:98/100`、`deletion.rs:36` 的用法 |
| 本地原子写是否 fsync 目录 | **误报，已撤回**：函数结尾 `capacity.rs:323` 调用了 `sync_parent_directory(parent)`，`:327-335` 打开目录并 `sync_all`（非 Unix 为空实现）。我的阅读截断在 `:314`，漏掉了收尾。 | `capacity.rs:315-340` |
| 缩略图缓存写入 | **缺陷**：缓存记录只做 `flush()`（:423）就 rename（:426），没有 `sync_all`，也没有目录 fsync。崩溃可能留下截断记录，读取方按字段解析时可能误判为有效缓存。 | `gallery.rs:415-428` |
| 锁毒化 | **确认存在**：`gallery.rs` 有 4 处 `expect("gallery queue poisoned")`，一次持锁 panic 后该存储缩略图永久 500，直到重启。其余 119 处未分类（需按文件区分测试与生产）。 | `gallery.rs:154/323/334/350` |

## 仍未分类（数据已采集，结论待定）

1. 全仓 123 处 `lock().unwrap()` 中，除 `gallery.rs` 已确认的 4 处外，其余需按"是否在热路径 + 是否可能 panic 持锁"逐个判断。
2. 112 处 `as u64` / `as usize` 需按来源类型判断是否可能截断；目前没有证据表明存在溢出。
3. S3 在不支持条件写的服务上的**具体降级行为**（拒绝 vs 无保护覆盖）未读 `upload.rs` 分支实现。

## 复核结论（第一次：已关闭的三条）

| 原疑点 | 结论 | 证据 |
|---|---|---|
| WebDAV 双重百分号编码绕过 `..` | **双重编码不成立**：`percent_decode` 只执行一次（`webdav_path.rs:74-90`），`%252e%252e` 解码后得到字面量 `%2e%2e`，不会变成 `..`。单次编码的 `%2e%2e` 会解码成 `..`，其拒绝发生在此函数之后的路径解析中。 | `webdav_path.rs:52` 唯一一次解码；`:42` 用 `security::same_authority` 校验 Destination 主机 |
| `storage_catalog` 反斜杠归一化可能放过 `..` | **不成立**：归一化是拒绝式的，`Component::ParentDir` 直接 `bail!("local mount paths cannot contain '..'")`；`comparable_path`（`:153` 反斜杠替换）只用于比较，不参与写入路径解析。 | `storage_catalog.rs:145` |
| `vipsthumbnail` 参数注入 | **不成立**：命令参数全部是字面量（`--size`、`640x640>`、`-o`、`result.part.jpg[Q=78,strip]`、`source.part`），用户文件名只作为 `current_dir` 下的固定文件名出现，不进参数列表。 | `gallery.rs:586-592`；像素检查在 `:581` 用 `checked_mul` |

## 仍未关闭（需逐行确认）

1. **S3 条件写在 MinIO/RustFS/OSS/COS 上的降级**：97 处命中里，存储路径的用法与 provider 能力分支未核对。
2. **本地原子写顺序**：`capacity.rs:291/303/310`、`gallery.rs:426` 的"写临时 → fsync 文件 → rename → fsync 目录"是否完整，未逐条核对。
3. **锁毒化**：全仓 123 处 `lock().unwrap()`，需区分测试与生产。
4. **计数截断**：112 处 `as u64` / `as usize`，需按来源类型逐个判断。

## 需本地验证的清单

1. `webdav_path.rs` 的百分号解码顺序与 `..` 校验。
2. `storage_catalog.rs` 反斜杠归一化是否拒绝式。
3. `vipsthumbnail` 参数是否以 `--` 隔断选项。
4. `Content-Disposition` 文件名编码是否防注入。
5. 会话/门槛令牌比较是否常量时间。
6. 管理路由是否全部经过 CSRF 中间件。
7. TOTP 重放与恢复码一次性。

## 批 4 复核（本轮，扫描 + 定点阅读）

| 位置 | 严重程度 | 问题与后果 | 修复方向 | 把握程度 |
|---|---|---|---|---|
| `capacity.rs:342` `impl Drop for CapacityReservation` | — | 预留未提交时在 `Drop` 里归还额度（`:342-346` 起），说明"future 被丢弃也要记账一致"这条有实现。 | 无需改动。 | 确定 |
| `direct_upload.rs:257`、`upload_batch.rs:161` 两个 `Drop` 守卫 | — | 会话完成、上传执行都有 RAII 守卫，取消时能清理。 | 无需改动。 | 确定 |
| 全仓 27 处 `tokio::select!`（可取消点） | 中（待验证） | 每个 `select!` 分支都可能在中途丢弃 future；需要逐点确认：临时文件、容量预留、上传票据在取消后都被守卫回收，而不是留在"半提交"状态。 | 逐个 `select!` 检查分支内是否有跨 await 的可变状态。 | 需验证 |
| 全仓 76 处 `Mutex`/`RwLock` 字段、102 处 admission 相关标识 | 中 | 没有集中的锁序说明或调试断言；跨模块嵌套取锁时死锁只能靠人工保证。 | 把"准入 → 编辑租约 → 配置发布"的固定顺序写成注释与 debug 断言。 | 需验证 |
| 全仓检索 `namespace_lock\|naming_lock\|serialize_names` 命中 0 | 中 | 上传侧"同一命名空间串行化编号"没有以显式锁命名出现，实现点未定位；批量的 `available_destination` 是"查空闲名再用"，与它不共用可见的锁，并发时可能选到同一编号，随后由后端以"目标已存在"失败。 | 先定位上传侧的串行化实现；若确实靠 `UploadBatchStore` 的互斥，则批量路径也应走同一把锁。 | 需验证 |

结论：取消安全不是空白——三类 RAII 守卫存在；但"所有 `select!` 分支都被守卫覆盖"这一点未验证。锁顺序与命名串行化是真正的空白，需要逐点阅读，扫描无法定论。

## 批 4 补充核实（外部复核后的本地确认）

| 疑点 | 结论 | 证据 |
|---|---|---|
| 命名串行化不存在 | **错，存在**：`planning_gate(namespace)`，用 Weak 引用按命名空间保存 `Arc<Mutex<()>>`，规划阶段持有到票据创建 | 外部复核：`upload_batch.rs:228`、`:1203`、`:1289`；测试 `planning_serializes_shared_names_but_not_unrelated_namespaces` |
| keep_both 是否会静默覆盖 | **进程内不会**：S3 侧由 `mutation_gate` 串行 + 先 HEAD 目标（存在即 Conflict），本地侧由 `create_new(true)` 强制；真正的条件头在 `s3_backend.rs:423-424`，OSS 走 `x-oss-forbid-overwrite`（`:434`）。 | `storage_transaction.rs:41/805/956`、`s3_backend.rs:423-434`、`copy.rs:41/68` |
| 证据引用更正 | 原报告引用的 `conditional_publish.rs:97-101` 是**启动时的能力探测**（metadata 带 `ycloud-conditional-probe`），不是发布路径；`commit.rs:111` 实为 `src/config/commit.rs`（配置原子写），与复制无关。两处已作废。 | 外部复核 + 本地确认 |
| `s3_backend/maintenance.rs:79` 的 deadline 包装 | 它会 `select!` 取消 `work` 本身；文件注释明确写"A deadline cancels reads/admission, never an issued write"，所以要成立必须**所有调用点的 work 都是读或准入** | `maintenance.rs:78-91`，注释在 `:96-98`；调用点审计尚未做 |
| `wait_for_recovery_work` 是否丢唤醒 | **未验证**：`recovery.rs:288` 只是转发到 `recovery_runtime.wait_for_work()`，真正的等待实现在 `recovery_runtime`，没读 | `recovery.rs:283-292` |
| `capacity.rs` 缺目录 fsync | **误报，已撤回**：`:323` 调 `sync_parent_directory`，`:327-335` 打开目录 `sync_all` | `capacity.rs:315-340` |

### 外部复核补充的批 4 结论（采纳）

- 非测试代码 `select!` 25 处，逐点判定全部"安全"或"较确定安全"（S3 上传取消后返回 `NotCommitted` + `CleanupState::Pending`，由恢复 worker abort；画廊子进程 `kill_on_drop(true)`）。
- 锁序：S3 为"生命周期准入 / 编辑守卫 → recovery_gate → 容量变更 → mutation_gate"，本地为"io 许可 → mutation"，调用点一致，无反序；但没有文档与断言，属低危。
- `recover_quiesced_uploads` 只取 `mutation_gate`，其互斥依赖调用方持有编辑守卫，是隐式约定，未在代码中写明。

## 批 3 复核：条件写降级路径（已读源码）

| 位置 | 严重程度 | 结论 | 证据 |
|---|---|---|---|
| `s3_backend/upload.rs:22-27` | — | 条件上传超过 4 GiB（S3 单对象原子复制上限）时**直接拒绝**，不降级。 | `CONDITIONAL_FILE_PUBLISH` 能力错误 |
| `s3_backend/upload.rs:28-33` | — | 阿里云 OSS 上带条件的上传**直接拒绝**，理由是"OSS 写入路径不能原子核验这些条件"。 | 能力名 `conditional_write` |
| `s3_backend/upload.rs:34-39` | — | 带 `If-` 时间条件的正式发布**直接拒绝**，要求改用 If-Match 版本标签。 | 同文件 |
| `s3_backend.rs:153` `conditional_publish_verified` + `:396` `capabilities::CONDITIONAL_FILE_PUBLISH` | — | 条件发布能力是**启动时探测并记住**的，未通过探测的后端不会被当成支持条件写。 | `capabilities.rs:6-9` 定义三种能力名 |
| `error.rs:410` | — | 能力缺失时返回结构化错误（`error.capability` 字段），前端可识别。 | `error.rs:410-416` |

**结论**：条件写没有"静默降级"路径，策略是 fail-closed——不支持或无法原子核验时直接拒绝并给出能力名。

因此 `keep_both` 的"不覆盖"保证是成立的：本地靠 `create_new`，S3 靠 `If-None-Match: *`，两者在能力不足时都会失败而不是覆盖。

仍需确认（低危）：`conditional_publish_verified` 的探测在哪些后端会失败、失败后是否会影响普通（无条件）上传——从代码看只影响条件上传，普通上传不受限。

## 批 3 更正（外部复核后）

原文"一律 fail-closed"说过头，按路径更正为三种情况：

| 路径 | 保护手段 | 后端忽略 If-None-Match 时 |
|---|---|---|
| 客户端带条件头的上传 | 启动探测（`upload.rs:401` 调 `verify_conditional_publish`）+ 能力错误 | 拒绝（fail-closed） |
| keep_both / 复制 / 移动的目标"必须新建" | `copy_file_locked` 持 `mutation_gate`，先 HEAD 目标，存在则 Conflict（`copy.rs:41/68`），再带条件头 | 进程内仍安全；外部写入者竞态时可能覆盖 |
| 源 > 4 GiB 的分片复制 | 仅 HEAD 预检（`multipart/copy.rs:22`） | 同上 |

结论：keep_both 的"不覆盖"在**进程内并发**下成立，与后端是否支持条件写无关；If-None-Match 只是防外部写入者的第二道防线。遗留风险仅限"不支持 If-None-Match 的后端 + 同时有外部程序写同一 key"，与设计文档已声明的"S3 替换非原子"同类，定低危。

另两条已关闭：`wait_for_work` 不丢唤醒（`recovery_runtime.rs:127` `Notify::notified()`，`:80/:120` 用 `notify_one` 存许可）；`.read()` 包装的调用点审计无正式写（唯一删除是 `deletion.rs:8-14` 的 `.ycloud-system/` 临时对象）。

## 批 4 遗留：整数截断审计（已分类）

全仓 `as u64` / `as usize` 共 112 处，按方向分类：

| 分类 | 数量 | 结论 |
|---|---|---|
| 窄化（`u64 as usize`、`u64 as u32`、`u32 as usize`、`i64 as u64`、`i32 as u64`、`u128 as …`，以及 `as u32/i32/i64`） | 有若干处（外部复核在非测试代码中数到十几处） | 均有界或有来源约束：分片号受 S3 10000 上限约束、relay 先 min/clamp、purpose 是内部短常量。**例外待确认**：`block_seconds: u64 as i64`（`auth/login.rs:54/124`、`admin_api.rs:880/888`），上界由 `validate_login_security_settings`（`config/validation.rs:78`）决定，未读 |
| `as u64`（来自 `usize` / `u32` / `u16` / `u8`） | 多数 | 64 位目标下安全 |
| `as u64`（来自 `as_millis()`，即 u128） | 少量（如 `api.rs:659`） | 理论上可截断，实际需运行约 5.8 亿年才溢出 u64 毫秒，可忽略 |
| `as u64`（来自 `num_seconds()`，即 i64 时长） | 少量（如 `auth.rs:277/457`） | 来源是正数常量，安全 |

结论：**没有发现可被外部输入触发的截断风险**。若将来支持 32 位目标，`usize as u64` 仍安全，但 `usize` 承载字节数的位置需要复查（32 位下本身就会限制文件大小）。

## 批 4 遗留：崩溃恢复（结构 + 测试证据）

审查深度：模块结构、入口与测试清单已读；`recover()` / `recover_replacement()` 的逐行行为未读完。

| 观察 | 证据 | 评价 |
|---|---|---|
| 恢复入口单一：`recover()`，内部依次读清单 → 处理替换 → 交接副本 | `recovery.rs:32`、`:107`、`:220`、`:363` | 入口集中，没有第二条恢复路径 |
| 日志读取有上限与校验 | `metadata.rs` 的 `MAX_JOURNAL_BYTES`、`MAX_RECOVERY_ENTRIES`；`decode_replace_journal`（`recovery.rs:388`） | 坏日志不会被无限读取 |
| 清理有共享预算，且跨批次有上限 | `DELETION_NODES_PER_PASS = 256`（`storage_transaction.rs:25`），测试 `:417`、`:454`、`:702` | 大删除债务不会一次拖垮恢复 |
| 冲突时先停、不做清理 | 测试 `:551`（conflicting copy cleanup ids stop）、`:677`（duplicate pending destinations stop） | 安全取向正确 |
| 校验阶段只读，已发布历史与待处理分离 | 测试 `:581` | 不会在校验时改动磁盘 |
| 发布收据：存在、可按操作 ID 读、可裁剪、目录同步失败不丢槽位 | `receipts.rs:52/92/224/252`，测试 `:266`（published receipt keeps its slot when directory sync fails） | 收据与发布顺序有测试兜底 |

结论：崩溃恢复不是临时补丁，而是"journal + receipt + 分轮预算 + 冲突先停"的完整设计，且有 8 条针对边界条件的测试。**未发现高危或中危问题。**

剩余（低危，需逐行读）：已由外部复核关闭两条——`recover_replacement()` 的顺序正确（先 `rooted_sync_parent` 再 `ensure_receipt`，无证据只记 Unknown）；`handoff_recovered_copy` 没有"删除源副本"动作，改为移入 trash 并同步两侧目录，重试幂等。

外部复核新发现（低危，需验证）：回滚恢复在"备份已 rename 回目标、upload 文件已删、日志未删"之后再次崩溃，重入时会被判成 `destination_exists && !upload_exists` → `was_committed = true`。非锚点日志记 Unknown（保守，无数据风险）；Linux 锚点日志因 inode 与锚点不符返回 `Conflict`，资源保留但该日志卡住，需人工处理。方向是 fail-safe。建议加故障注入测试，在"upload 已删、日志未删"处停下重入，断言为 `NotCommitted` 而非 `Conflict`。

## 批 1 复核（扫描 + 定点阅读）

| 位置 | 严重程度 | 结论 | 证据 |
|---|---|---|---|
| `src/totp.rs:21-25` `TotpReplayStore::consume` | — | 重放防护成立：`Mutex` 内先裁剪窗口（只保留 `counter±2`），再 `insert` 返回是否首次，重复计数器直接拒绝。 | `totp.rs:21-25` |
| `src/totp.rs:39-46` 恢复码生成 | 低（待验证） | 生成用去混淆字符表，但**消费路径未读**：需确认恢复码使用后被删除且不允许重复使用。 | `totp.rs:39-46` |
| `src/auth/session.rs:58-78` 单会话替换 | — | 写锁内 `retain` 掉同 principal 的旧会话再插入，并发登录不会留下两个有效会话。 | `session.rs:62-76` |
| `src/auth/session.rs:79-93` | — | 读取时逐次校验 24 小时 TTL；令牌是 122 位 UUID，`HashMap` 查找虽非常量时间，但时序攻击不可行。 | `session.rs:8`、`:79-93` |
| `src/login_security.rs:137-138` | 低（待验证） | 失败计数与事件日志在 `tokio::sync::Mutex` 后，更新串行化。**需确认没有"先读后写"的跨 await 窗口**：检查与记录若分两次取锁，并发失败可能只记一次。 | `login_security.rs:137-138` |
| `src/security.rs:177-200` `csrf_middleware` | 低（待验证） | CSRF 校验存在并在 `app.rs:141/219` 两个路由组挂载；`csrf_request_allowed` 覆盖"有 Cookie/Origin/Sec-Fetch-Site 即要求同源"，登录也在内。**未验证**：所有会改状态的管理路由是否都在这两个组内。 | `security.rs:177-200`、`app.rs:141/219` |

结论：批 1 没有发现高危或中危问题。四条待验证：恢复码一次性、登录失败计数的读写窗口、CSRF 路由覆盖、`block_seconds` 的 `u64 as i64` 上界（`config/validation.rs:78`）。

### 批 1 收尾（继续阅读后）

| 项 | 结论 | 证据 |
|---|---|---|
| 恢复码一次性 | **成立**：验证通过后把对应哈希从配置中删除，不能复用 | `auth.rs:241-245` |
| CSRF 路由覆盖 | **成立**：`api_routes`（public + protected）与 `admin_routes` 各自都套了 `csrf_middleware`；未套的只有 GET 页面与 WebDAV（Basic 认证，不依赖 Cookie） | `app.rs:135-142`、`:217-220` |
| 登录失败计数读写窗口 | **关闭，无缺陷**：三个登录入口（管理员/用户 `auth.rs:166`、站点门槛 `auth.rs:412`、WebDAV `webdav/access.rs:44`）都先取按 (入口, IP) 分条带的 `login_attempts` 锁，从 `is_blocked` 一直持有到 `record_failure` 返回，中间包住 argon2 校验。同 IP 并发被串行化，不会比阈值多出一次。原报告"分两次取锁、可能略超阈值"的说法错误，已撤回 | `auth.rs:166`、`:412`、`webdav/access.rs:44` |
| `block_seconds` 的 `u64 as i64` 上界 | **关闭**：`validate_login_security_settings` 限制在 5 分钟到 24 小时（`5*60..=24*60*60`），远小于 `i64::MAX`，`as i64` 安全 | `config/validation.rs:355-379` |

批 1 状态：**认证与会话主线已读，未达 100%**。剩余未读六项：

1. 主密钥：生成、文件权限、`YCLOUD_CONFIG_KEY` 覆盖、是否可能进日志
2. 可信代理：`ClientIp` 的来源，`X-Real-IP` 只信任精确匹配的对端，重复值与非法值处理（决定所有限流能否被绕过）
3. 配置迁移（schema 16）、原子写、备份文件、初始凭据文件删除时机
4. 密码哈希参数；站点密码与管理员密码的独立性
5. 流量账本：损坏恢复、整数溢出、`doctor traffic` 的只读保证
6. `admin_routes` 的实际成员是否都在 CSRF 组内（`app.rs:144-220` 未逐个核对）

设计观察（低危，需验证）：管理员登录按 IP 限流，没有账号维度的全局上限；多个 IP 分布式猜同一管理员密码时，每个 IP 各有一份阈值。有 TOTP 与 argon2 并发信号量兜底，实际风险低，但属取舍。用户账号走 `LoginEntry::Account.fixed_policy()`，是否按账号计数未验证。

### 可信代理与客户端地址（已读，逻辑链完整）

1. 真实对端来自 `ConnectInfo<SocketAddr>`；没有连接信息时，仅当 `bind_address` 是回环才用绑定地址兜底，否则直接 403（`security.rs:33-44`）。
2. 先校验 `Host`：`single_header(HOST)` 只接受唯一值，与 `public_host` 用 `same_authority` 比较——拒绝带 `@` 的 authority、主机名大小写不敏感、端口缺省按默认端口（`security.rs:120-134`、`:100-118`）。
3. 取客户端地址（`security.rs:83-98`）：**只有当对端 IP 精确出现在 `trusted_proxy_ips` 且请求带 `x-real-ip` 时**才采信该头；否则一律用真实对端。采信时必须通过 `single_header`（多值即拒绝），再解析为 IP 并 `to_canonical()` 归一化（IPv4-mapped IPv6 归一），非法值 400。
4. 结果写入请求扩展 `ClientIp`（`:50`），后续限流、审计都取它。

对应测试 `:374-430` 覆盖：不可信对端忽略该头、可信对端要求唯一且合法的地址、IPv4-mapped 归一、重复头被拒、中间件把地址传给下游。

结论：**限流与审计的地址来源不可被外部伪造**，前提是 `trusted_proxy_ips` 只配置真实反代地址且该反代覆盖写入 `X-Real-IP`。这是部署约束，不是代码缺陷。

## 轮 1：信任边界（发现表）

| 位置 | 严重度 | 一句话 | 修复方向 |
|---|---|---|---|
| `config/secret_store.rs:199-211/341/354` | — | **误报，已关闭**：密钥文件 `create_new(true)` + `options.mode(0o600)` 同一调用创建，无"先建后改"窗口；目录 `0o700`、已存在文件 `0o600` 修正、`AlreadyExists` 转读已有密钥、写完 `sync_all`；Windows 走 DPAPI | 无 |
| 全仓 `tracing::*` 含 password/token/secret/key | — | 唯一命中是 `listing.rs:250` 的 S3 key 日志，与凭据无关；未发现密钥进入日志 | 无 |
| `app.rs:135-142`、`:217-220` | — | 全部改状态路由都在 CSRF 层内：`api_routes`=public+protected，`admin_routes` 单独套；登录请求也检查 | 无 |
| `file_access.rs:118-157` `resolve_share` | — | 顺序是"存储 ID 合法 → 后端就绪 → 存储 browse 权限 → 401/403"；文件夹锁不在这里，由调用方按路径检查（`list_files`、`directory_changes` 都已做） | 无 |
| `security.rs:83-98` `request_client_ip` | — | 只有对端 IP 精确命中 `trusted_proxy_ips` 且 `x-real-ip` 唯一合法时才采信，否则用真实对端；IPv4-mapped 归一，非法值 400 | 无（部署须保证反代覆盖写该头） |
| 管理员登录限流 | 低（取舍） | 按 IP 分条带，无账号维度全局上限；多 IP 分布式猜测时各有一份阈值，有 TOTP 与 argon2 并发信号量兜底 | 可选：给管理员加账号级失败上限 |

轮 1 结论：无高危、无中危、无确证缺陷（主密钥权限为误报）。

## 轮 2：文件出入口（发现表）

| 位置 | 严重度 | 一句话 | 修复方向 |
|---|---|---|---|
| `webdav.rs:167-181` `propfind_depth` | — | `Depth` 严格解析；目录 + `Infinity` 直接拒（`DavError::FiniteDepth`），不静默降级 | 无 |
| `webdav_path.rs:42`、`webdav.rs:391-402` | — | `Destination` 用 `same_authority` 校验主机；`Overwrite` 头按 T/F 解析 | 无 |
| `webdav.rs:31/103` | — | **已关闭**：`MAX_CONTROL_BODY_BYTES = 64 KiB`，PROPFIND 等控制请求在解析 XML 前用 `to_bytes(body, 64 KiB)` 读取并受限；JSON 路由 32 KiB、批量 256 KiB | 无 |
| `archive.rs:185-207` | — | ZIP 下载在**流式阶段**累计 `total_bytes` 并与 `max_archive_bytes`/`max_archive_entries` 比较，不是先建后查 | 无 |
| `storage/response.rs:208-249` | — | **已关闭**：`filename="..."` 只保留 ASCII 字母数字与 `.` `-` `_`，其余换成 `_`；`filename*` 按 RFC 5987 只放行 attr-char，其余百分号编码；`HeaderValue::from_str` 失败兜底为 `attachment; filename="download"`，不 panic。仅缺特殊字符测试 | 补一条含 `"`、`;`、换行的用例 |

## 轮 3：前端（发现表）

| 位置 | 严重度 | 一句话 | 修复方向 |
|---|---|---|---|
| 全仓 `v-html` | — | **0 处**；`innerHTML` 只出现在测试文件里 | 无 |
| `AdminView.vue:96`、`FilePreviewDialog.vue:75/81`、`GalleryLightbox.vue:182`、`PreviewView.vue:54` | — | 动态 `:href` 都来自内部路由或 API 生成的下载地址，点击走 `@click.prevent` 调函数，不直接跳转 | 无 |
| 全仓 `localStorage`/`sessionStorage` | — | 只存 `ycloud-stay-signed-out` 标志，**没有令牌或密码** | 无 |
| `useUploadQueue.ts` 状态机 | 未读 | 取消/重试/并发路径未逐行读 | 后续补 |

## 轮 4：S3 与锁（发现表）

| 位置 | 严重度 | 一句话 | 修复方向 |
|---|---|---|---|
| 全仓 `lock().unwrap()` 分组 | 低 | 123 处里 **97 处在测试文件**（`tests.rs` 55、`protocol_tests.rs` 30、`copy_tests.rs` 8、`overwrite_tests.rs` 4）；生产代码集中在 `recovery_runtime.rs` 4、`journal.rs` 3、`object_read.rs` 3、`persistence.rs` 3 等，合计约 20 处 | 逐个改为容忍毒化，优先热路径 |
| `s3_backend/multipart.rs:78`、`recovery.rs:374/389` | — | 分片失败/恢复会 abort，孤儿会话有回收路径 | 仍需验证"创建响应丢失"窗口 |
| `storage_transaction.rs` 日志版本 | — | 存在多版本（version 5 / 2 / 1），说明有兼容分支 | 新增字段时必须升版本并保留旧分支 |

## 汇总

- 确证缺陷：`gallery.rs` 4 处锁 `expect`（中）、`gallery.rs:423` 缓存只 flush（低）、keep_both 并发撞号无重试（低）、回滚恢复二次崩溃返回 Conflict（低，待验证）。
- 中危数：**0**（主密钥权限为误报，已关闭）。
- 已关闭：WebDAV 双重编码、反斜杠归一、vipsthumbnail 注入、CSRF 覆盖、日志泄露、会话单实例、TOTP 重放、恢复码一次性、条件写降级（按路径分三种）、原子写顺序、整数截断、崩溃恢复结构、主密钥权限、PROPFIND 请求体上限、Content-Disposition 注入、前端 XSS 面与敏感信息存储。

## 路线图修订（最终）

| 顺序 | 项 | 严重度 |
|---|---|---|
| 1 | `gallery.rs:154/323/334/350` 改 `unwrap_or_else(PoisonError::into_inner)` | 中 |
| 2 | `gallery.rs:423` `flush()` 改 `sync_all()` | 低 |
| 3 | `batch_operations.rs:202` keep_both 撞号时用有界重试换下一个编号 | 低 |
| 4 | 回滚恢复二次崩溃的故障注入测试（断言 `NotCommitted` 而非 `Conflict`） | 低 |
| 5 | 生产代码约 20 处 `lock().unwrap()` 逐个改容忍毒化 | 低 |
| 6 | 补测试：`Content-Disposition` 特殊字符、可信代理 | 低 |
| 7 | 读 `useUploadQueue.ts` 状态机（取消/重试/并发） | 空白 |

已从路线图删除：WebDAV 双重解码、`vipsthumbnail` 参数隔断（均已在复核中证明不成立）；"批 3/批 4 逐行审查"已完成，拆分为上述具体项。

## 修复路线图

| 顺序 | 项 | 依赖 |
|---|---|---|
| 1 | `gallery.rs` 4 处锁 `expect` 改为容忍中毒 | 无 |
| 2 | WebDAV 双重解码 / 反斜杠归一化（高，待验证） | 需先读 `webdav_path.rs` |
| 3 | `vipsthumbnail` 参数隔断 | 无 |
| 4 | 令牌比较改常量时间 | 需先确认现状 |
| 5 | `Lagged → resync` 回归测试；删除前端 300ms 合并 | 无 |
| 6 | 批 3、批 4 逐行审查 | 需分批进行 |
