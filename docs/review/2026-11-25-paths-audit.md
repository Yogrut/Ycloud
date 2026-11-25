# 路径与存储访问审查（2026-11-25，第二轮）

范围：`src/storage.rs`、`src/storage/linux_root.rs`、`src/storage/path.rs`、
`src/storage_backend.rs`、`src/webdav.rs`、`src/webdav_path.rs`、`src/s3_backend/`。

## 1. 路径规范化规则（已用测试固定）

实现：`storage/path.rs:132-151`。新增测试
`storage/path.rs::name_tests::relative_paths_are_normalized_without_resolving_traversal`
覆盖下表：

| 输入 | 结果 |
| --- | --- |
| `a/b` | `a/b` |
| `/a//b/` | `a/b` |
| `./a`、`a/./b` | `a`、`a/b` |
| `a//b`、``、`/`、`//` | `a/b`、`` |
| `..`、`../x`、`a/../b` | 400（不解析，直接拒绝） |
| `a\b`、`a\0b` | 400 |
| `C:/x`、`a/b:c` | 400（组件内含 `:`） |
| `.ycloud-system`、`a/.YCLOUD-SYSTEM/b` | 400（系统目录，大小写不敏感） |

结论：`..` 不会被解析，只会被拒绝；反斜杠、NUL、冒号、系统目录均拒绝。

## 2. 本地后端与 S3 后端的一致性

- 本地：`storage/listing.rs:33,111`、`object_read.rs:22`、`upload.rs:117` 等均先
  `normalize_relative`。
- S3：`s3_backend/keyspace.rs:36,44` 同样走 `normalize_relative` 后再拼键。
- 因此 API 面无法产生含 `..` 的 S3 键；外部工具写入的字面 `..` 键经由 API
  不可达（返回 400），不会出现"本地拒绝、S3 放行"的差异。代价是这类对象
  在 Web 端不可见，属于可接受限制。

## 3. 符号链接与逃逸

- 根句柄打开使用 `openat2`，flags 为
  `BENEATH | NO_SYMLINKS | NO_MAGICLINKS`（`storage/linux_root.rs:19-21`）。
- `validate_existing` 与 `open_file_for_read` 在拿到 metadata 后再次拒绝
  `is_symlink()`（`linux_root.rs:46-105`）。
- 已有测试：`root_handle_rejects_symlinks_and_survives_path_replacement`
  （`linux_root.rs:886`）。

**待补**：逐操作矩阵（列表、下载、上传、重命名、删除、复制各对符号链接的
状态码）尚未写成测试；需要 Linux 环境，计划放在下一轮开头补
`storage/linux_root.rs` 的 Linux-only 测试。

## 4. 文件操作的后端入口

所有请求路径最终都调用 `AppState::storage_backend`（停用存储在此拒绝，
`state.rs:208-218`）：

`api.rs:335,525,562,711,753,778`、`archive.rs:171,341`、
`batch_operations.rs:110`、`direct_upload.rs:146`、`directory_size.rs:42`、
`gallery.rs:394`、`upload_batch.rs:1206`、`webdav.rs:117`。

唯一的例外是后台 retire 任务（`bootstrap.rs:91` 的 `state.backends.cached`），
它不在请求路径上；停用存储时注册表是否移除该条需要确认（R2-4）。

## 5. WebDAV 路径处理

- 源路径：`Path<String>` 由 axum 解码一次（`webdav.rs:87,109`），随后交给后端
  `normalize_relative`。顺序是"先解码、后规范化"。
- `Destination`：`Uri::path()` 保留百分号编码，`parse_destination` 解一次
  （`webdav_path.rs:26-53,74-90`），再由后端规范化。
- 双重编码（`%252e%252e`）只会得到一个字面 `%2e%2e` 文件名，不会变成 `..`。
- MOVE/COPY 的目标必须同 authority 且以 `/dav/<share>` 开头
  （`webdav_path.rs:35-51`），跨 share 目标返回 403；只读挂载对所有写方法统一
  403（`webdav/access.rs:83-85`）。同一挂载内源和目标共享同一份认证，因此
  不存在"源有权限、目标没权限"的分叉。

## 6. Windows 行为

- 大小写：锁比较在 Windows 上大小写不敏感（`config.rs:197-205`），与 NTFS 一致。
- **尾点/尾空格别名（发现）**：Windows 会把组件末尾的 `.` 和空格去掉再解析。
  在本机实测 `Test-Path "<root>\locked."` 与 `"<root>\locked "` 都返回 True，
  即都能命中 `locked` 目录。而 `normalize_relative` 保留这些字符，
  `path_is_same_or_descendant` 按组件精确比较，不 trim。因此 Windows 部署上
  `path=locked.` 或 `path=locked%20` 可以通过锁检查，随后被文件系统解析到被锁
  目录（详见 R2-1）。
- 保留设备名：`validate_name` 未拒绝 `CON`/`NUL` 等；本机 `Test-Path` 对
  子目录下的 `CON` 返回 False，未观察到设备访问，暂列为疑问，不当作结论。

## 发现

| 编号 | 类别 | 位置 | 触发条件 | 证据 | 建议 |
| --- | --- | --- | --- | --- | --- |
| R2-1 | 锁绕过（Windows） | `storage/path.rs:132-151`、`config.rs:197-205` | 已修（见第 7 节） |
| R2-2 | 测试缺口（已补） | `storage/path.rs:200+` | — |
| R2-3 | 行为限制 | `s3_backend/keyspace.rs:36,44` | 文档说明即可 |
| R2-4 | 后台任务 | `bootstrap.rs:91` | 已确认安全：停用时 `state.rs:722-730` 先失效再移除；`cached` 返回 None 时任务直接返回，进行中的恢复受 interrupt/Conflict 保护 |
| R2-5 | 疑问 | `storage/linux_root.rs` | 待补 Linux-only 六操作测试 |
| R2-6 | 疑问 | `storage/path.rs:132` | 保持疑问，已加 Windows 测试固定"当前接受设备名"的行为 |

R2-1 只在 Windows 部署上成立；Linux/容器部署（尾点、尾空格是不同文件名）不受影响。

## 7. R2-1 修复（Windows）

修复内容：

- `normalize_relative`：Windows 上组件以 `.` 或空格结尾 → **400**，到不了文件系统。
- `validate_name`：Windows 上同样拒绝，避免"创建 `a.` 实际存成 `a`"的静默改名。
- `path_component_eq`（Windows）：比较前去掉组件末尾的点/空格，并改用 Unicode
  小写比较，`Ärzte` 与 `ärzte` 视为同一目录（锁返回 403，而不是绕过）。

实测结果：

- 非 ASCII 大小写：`Test-Path` 对 `Ärzte`、`ärzte`、`ÄRZTE` 均为 True，
  旧的 `eq_ignore_ascii_case` 会绕锁，已改为 Unicode 折叠。
- 8.3 短名：本机 `dir /x` 未生成短名（该卷关闭 8.3），`LOCKED~1` 无法复现，
  不改代码；建议部署文档注明关闭 8.3（`fsutil 8dot3name query`）。
- 设备名：`CON` 未观察到设备访问，新增测试固定当前行为。

测试：`storage/path.rs` 两个 Windows 测试、`config.rs` 的
`windows_folds_case_and_trailing_aliases_before_comparing`、
`api.rs::permission_matrix_tests::windows_alias_spellings_cannot_bypass_folder_locks`
（`locked.`/`locked%20` → 400；`LOCKED`/`ärzte`/`ÄRZTE` → 403）。

Linux 与 Docker 部署不受影响：以上判断都在 `#[cfg(windows)]` 下。

macOS 默认文件系统同样大小写不敏感，但 CI 与发布目标只有 Linux/Windows，
暂不覆盖，本文档注明即可。
