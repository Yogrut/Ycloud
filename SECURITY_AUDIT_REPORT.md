# Ycloud 安全审查执行报告

> 本报告始于 2026-08-30 对原始功能基线 `4fa46f69c070` 的预审，并记录安全加固代码基线 `7994d40d4fe5` 与容器部署基线 `23b4bff998d2` 的阶段性证据。它不是安全认证，也不是 Release 签字。由于部分隔离动态用例和实际 Linux 容器验收仍不完整，本报告不作“通过”结论。

正式生产审核范围为 Linux 原生进程、Docker 和 Docker Compose。Windows 仅用于开发编译与本机回归，其服务安装、ACL/DACL 和生产运维不属于发布门禁。

## 1. 审查基线

| 项目 | 结果 |
| --- | --- |
| 分支 | `main` |
| 原始功能基线 | `4fa46f69c07042ae01212eba662594b2877144b9` |
| 安全加固代码基线 | `7994d40d4fe5c14670f1f145e034aab1742704e3` |
| 容器部署基线 | `23b4bff998d29ad687d6d37c29b741a3b6e42243` |
| 基线状态 | 安全加固与 Docker/Compose 部署配置均已提交；容器运行结论仍待 Linux CI 或部署机证据 |
| `git diff --check` | 通过；仅报告 Git 的 LF/CRLF 提示 |
| Rust | `rustc 1.96.1` / `cargo 1.96.1` |
| Node/npm | `node v24.19.0` / `npm 11.17.0` |
| Windows 开发测试产物 SHA-256 | `D3BD08770FE050B51C80D3BE9CEB5B77286259233F005F37B71F0B3A9235FFFD`；不作为发布产物 |
| 前端 JS SHA-256 | `131E1559155F25AC823FA580575C605490B97350D67948A3274B08048868E6E5` |
| 前端 CSS SHA-256 | `A7E01BE90158E0811983076F149412C1789F543CC12E643320101E1CA7921342` |

## 2. 已执行检查

| 检查 | 结果 | 说明 |
| --- | --- | --- |
| `cargo fmt --all -- --check` | 通过 | 无格式差异 |
| `cargo clippy --locked --all-targets -- -D warnings` | 通过 | 无 Clippy 警告 |
| `cargo test --locked` | 通过 | 安全加固代码基线 107 通过、0 失败、1 ignored；ignored 为需显式配置的真实 S3 smoke |
| 腾讯 COS 真实 smoke | 通过 | `s3_backend::tests::s3_compatibility_smoke`：1 通过、0 失败；使用成都地域隔离 Bucket 和随机 Prefix |
| 阿里 OSS 真实 smoke | 通过 | 修复条件写和 SDK checksum framing 兼容性后：1 通过、0 失败；使用成都地域隔离 Bucket 和随机 Prefix |
| RustFS 真实 smoke | 通过 | 局域网 Linux RustFS `1.0.0-beta.12`：健康探测通过，完整 smoke 1 通过、0 失败；使用隔离 Bucket 和随机 Prefix |
| MinIO 真实 smoke | 通过 | 局域网 Linux MinIO：健康探测通过，完整 smoke 1 通过、0 失败；使用隔离 Bucket 和随机 Prefix |
| 通用 S3 配置分支 smoke | 通过 | 使用 `s3_compatible` Provider 对同一 MinIO 隔离 Bucket 执行：1 通过、0 失败；不代表 AWS S3 或任意未测试实现已通过 |
| MinIO Multipart/错误凭据专项 | 通过 | 流式 65 MiB Multipart、逐段下载校验、大文件复制/移动/删除及错误 Secret 拒绝全部通过；1 通过、0 失败 |
| RustFS Multipart/错误凭据专项 | 通过 | 流式 65 MiB Multipart、逐段下载校验、大文件复制/移动/删除及错误 Secret 拒绝全部通过；1 通过、0 失败 |
| MinIO 客户端断流/Multipart 残留专项 | 通过 | 首个 64 MiB 分片提交后注入请求体错误；目标不可见、容量为零且未完成 Multipart 列表为空；1 通过、0 失败 |
| RustFS 客户端断流/Multipart 残留专项 | 通过 | 首个 64 MiB 分片提交后注入请求体错误；目标不可见、容量为零且未完成 Multipart 列表为空；1 通过、0 失败 |
| MinIO TCP 中断/恢复清理专项 | 通过 | 回环故障代理在首个 UploadPart 传输中断开连接；恢复后目标不可见、容量为零且未完成 Multipart 列表为空；1 通过、0 失败 |
| RustFS TCP 中断/恢复清理专项 | 通过 | 回环故障代理在首个 UploadPart 传输中断开连接；恢复后目标不可见、容量为零且未完成 Multipart 列表为空；1 通过、0 失败 |
| MinIO 持续断网/延迟恢复专项 | 通过 | UploadPart 中断后保持代理离线，使即时 Abort 失败；恢复网络后根据持久化会话终止远端 Multipart，目标不可见且容量归零；默认线程栈下 1 通过、0 失败 |
| RustFS 持续断网/延迟恢复专项 | 通过 | 与 MinIO 相同的持续断网、即时 Abort 失败和持久化恢复流程；默认线程栈下远端 Multipart 清空、目标不可见且容量归零；1 通过、0 失败 |
| `cargo build --locked --release` | 通过 | 生成 Windows 本机开发测试可执行文件，不作为正式发布产物 |
| `npm ci` | 通过 | 依照 lockfile 安装 |
| `npm run check` | 通过 | lint、27 个测试文件/110 项测试、typecheck、build 均通过 |
| `cargo deny check bans licenses sources` | 通过 | 许可证、来源和禁用依赖通过；存在重复依赖警告 |
| `npm audit --audit-level=moderate` | 通过 | 0 vulnerabilities |
| `cargo audit` | 通过 | 联网更新 RustSec 官方公告库后扫描 253 个锁定依赖，无漏洞命中；数据库 HEAD `b331df68b3ed`，提交时间 2026-08-29 |
| 隔离 HTTP Host/CSRF/代理检查 | 阶段通过 | 本地模式：健康 200、恶意 Host 403、匿名管理接口 401、缺失同源信息 403；代理模式：正确代理头 200、缺失 Proto 400、恶意 Host 403、多值转发头 400 |
| Cookie 属性检查 | 通过 | 本地 Gate Cookie 为 HttpOnly/SameSite=Strict 且无 Secure；HTTPS 代理模式增加 Secure |
| 票据/TOTP/密钥专项回归 | 通过 | 上传批次 3 项、归档主体绑定 1 项、TOTP 重放 1 项、安全边界 9 项、S3 凭据加密和 TOTP 主密钥测试均通过 |
| 静态敏感信息扫描 | 未发现匹配 | 扫描了提交文件中的常见 Access Key、私钥和明文 Secret 模式；不替代 Git 历史专用扫描器 |

## 3. 环境与动态验证状态

### 3.1 RustSec 公告数据库

2026-08-31 已获准联网更新 RustSec 官方公告数据库，数据库 HEAD 为 `b331df68b3ed0e99594d259040bdcb9de3c7c8a4`（提交时间 2026-08-29）。`cargo audit 0.22.2` 扫描 `Cargo.lock` 中 253 个依赖，无漏洞命中。该结论仅绑定本次锁文件和上述公告快照；后续 Release 仍需重新联网执行。

### 3.2 S3 真实环境

2026-08-30 已在腾讯云成都地域的隔离 Bucket `ycloud-smoke-1321907833` 上执行 `s3_backend::tests::s3_compatibility_smoke`。测试使用官方 HTTPS 区域 Endpoint、虚拟主机寻址和每次运行生成的随机 Prefix，列举、激活探测、上传、下载、覆盖、文件复制/移动/删除、目录复制/移动/删除及最终容量清零均通过。测试完成后临时 DPAPI 凭据文件已删除。

2026-08-30 已在阿里云成都地域的隔离 Bucket `ycloud-smoke-14124124` 上执行同一真实 smoke。首次运行发现 RAM 未授权；授权后又发现 OSS 不支持标准 S3 条件 `PutObject`，以及 AWS SDK 默认可选 checksum trailer 与 OSS 不兼容。修复后，完整 smoke 1 项通过、0 失败，测试前缀清零，临时 DPAPI 凭据文件已删除。

2026-08-30 已对局域网 Linux 服务器上的 RustFS `1.0.0-beta.12` 执行健康检查和同一真实 smoke。测试使用 S3 API Endpoint `http://192.168.2.37:9000`、`us-east-1`、路径寻址、隔离 Bucket `ycloud-smoke` 和随机 Prefix；服务就绪、列举、激活探测、上传、下载、覆盖、文件复制/移动/删除、目录复制/移动/删除及最终容量清零均通过。完整 smoke 1 项通过、0 失败，临时 DPAPI 凭据文件已删除。

2026-08-30 已对局域网 Linux 服务器上的 MinIO 执行健康检查和同一真实 smoke。测试使用 S3 API Endpoint `http://192.168.2.37:9002`、`us-east-1`、路径寻址、隔离 Bucket `yogrut-test` 和随机 Prefix；列举、激活探测、上传、下载、覆盖、文件复制/移动/删除、目录复制/移动/删除及最终容量清零均通过。完整 smoke 1 项通过、0 失败，耗时 5.54 秒，临时 DPAPI 凭据文件已删除。

2026-08-30 已将 Provider 切换为 `s3_compatible`，对上述 MinIO Endpoint 和 Bucket 再次执行完整 smoke；1 项通过、0 失败，耗时 5.13 秒，临时 DPAPI 凭据文件已删除。该结果证明 Ycloud 通用 S3 配置分支可与这一 MinIO 实现正常协作，不代表 AWS S3 或任意未测试的第三方 S3 实现已经验收。

2026-08-30 已对上述 MinIO 执行第一轮专项测试。测试从分块流生成 65 MiB 数据，跨过 Ycloud 的 64 MiB Multipart 阈值；上传后以流式下载逐段校验内容，并完成大文件复制、移动、删除和最终容量清零。同时以正确 Access Key ID 配合随机错误 Secret 验证服务拒绝访问。完整测试 1 项通过、0 失败，耗时 13.65 秒，临时 DPAPI 凭据文件已删除。

2026-08-30 已在 RustFS Endpoint `http://192.168.2.37:9000` 的新隔离 Bucket `yogrut-test` 上执行相同专项测试。65 MiB Multipart、流式下载逐段校验、大文件复制/移动/删除、最终容量清零及错误 Secret 拒绝全部通过；完整测试 1 项通过、0 失败，耗时 9.43 秒，临时 DPAPI 凭据文件已删除。

2026-08-30 已在 MinIO 上执行客户端断流专项测试。测试在首个 64 MiB 分片成功提交后让请求体返回错误，并验证目标文件不可见、用户数据容量为零、该随机 Prefix 下未完成 Multipart 列表为空。完整测试 1 项通过、0 失败，耗时 9.23 秒，临时 DPAPI 凭据文件已删除。

2026-08-30 已在 RustFS 上执行相同客户端断流专项测试。目标文件不可见、用户数据容量为零、该随机 Prefix 下未完成 Multipart 列表为空；完整测试 1 项通过、0 失败，耗时 5.79 秒，临时 DPAPI 凭据文件已删除。

2026-08-30 已在 MinIO 上执行传输层中断专项测试。测试经仅监听本机回环地址的临时 TCP 故障代理转发，在首个 UploadPart 请求传输部分数据后断开连接，随后恢复正常转发；Ycloud 返回上传失败并完成 Abort，目标文件不可见、用户数据容量为零、未完成 Multipart 列表为空。完整测试 1 项通过、0 失败，耗时 8.00 秒，临时 DPAPI 凭据文件已删除。

2026-08-30 已在 RustFS 上执行相同传输层中断专项测试。Ycloud 返回上传失败并完成 Abort，目标文件不可见、用户数据容量为零、未完成 Multipart 列表为空；完整测试 1 项通过、0 失败，耗时 4.46 秒，临时 DPAPI 凭据文件已删除。

2026-08-30 已在 MinIO 上执行持续断网专项测试。测试在首个 UploadPart 传输部分数据后断开连接并保持代理离线，使上传失败后的即时 Abort 同样无法连接；网络恢复后，Ycloud 从 `.ycloud-system/multipart-sessions/` 读取受前缀和结构校验约束的会话记录，终止远端 Multipart 并删除日志。目标文件不可见、用户容量为零且未完成 Multipart 列表为空；默认 Windows 测试线程栈下 1 项通过、0 失败，耗时 9.59 秒，临时 DPAPI 凭据文件已删除。测试期间同时发现会话日志 PUT 与 UploadPart 组合 future 会使原测试线程栈溢出，现已通过独立 Tokio 任务边界缩小调用栈，并由本次默认栈实测确认修复。

2026-08-31 已在 RustFS 上以默认 Windows 测试线程栈复验相同持续断网场景。测试启动时先终止隔离 `ycloud-smoke/` 前缀下前次异常测试留下的未完成分片，再执行 UploadPart 中断、持续离线、即时 Abort 失败和网络恢复。持久化会话恢复成功，目标文件不可见、用户容量为零且未完成 Multipart 列表为空；1 项通过、0 失败，耗时 5.46 秒，临时 DPAPI 凭据文件已删除。

阿里适配使用原生 `x-oss-forbid-overwrite` 保护首次写入；事务更新和删除在进程内共享写锁下先校验 ETag。由于 OSS 缺少等价的标准条件更新/删除，本保证不跨 Ycloud 进程，同一 Bucket/Prefix 必须保持单写实例。MinIO 与 RustFS 已有 Multipart、客户端请求体断流、单次 TCP 断连恢复，以及即时 Abort 同样断网后的持久化恢复证据。公有云同类传输故障、限流、凭据轮换及厂商故障恢复仍待测试。

代码已有 S3 测试连接探测：列举、随机内部前缀写入、HEAD/ETag、读取、服务端复制和删除，并在失败时清理临时对象。该探测不等于完整的大文件、Multipart、故障恢复和厂商兼容性验收。

### 3.3 动态安全矩阵

SECURITY_AUDIT.md 中 T-01 至 T-41 的适用隔离动态用例仍未全部执行。2026-08-31 已用临时目录和随机端口启动两个真实 Ycloud 进程，分别验证本地模式和 HTTPS 代理模式的代表性 Host、CSRF、匿名管理接口、转发头及 Cookie 属性；同时补跑票据主体绑定、TOTP 重放和密钥静态加密回归。尚缺专用管理员/普通账号完整权限矩阵、实际 Nginx/Caddy、公网 HTTPS、Linux 路径 TOCTOU、预览隔离和恢复演练。Windows ACL 用例已按正式平台范围标记为不适用。

### 3.4 Linux 与容器

容器部署基线新增多阶段 Linux `Dockerfile`、默认仅发布宿主机回环地址的 `compose.yaml`、独立 HTTPS 代理覆盖、非 root UID/GID 10001、只读根文件系统、Capability 全移除、`no-new-privileges`、PID 上限、健康检查和 Compose Secret 文件注入。CI 已加入 Compose 模型解析、镜像构建、用户元数据检查以及只读容器 `/api/ready` 验证。

当前 Windows 审核机没有 Docker 或 WSL，因此本轮不能把上述容器配置记为运行通过；必须等待 Linux CI 或部署机实际执行并记录镜像 Digest、容器日志、卷权限和健康状态。

## 4. 需要优先验证的静态候选项

以下来自 SECURITY_AUDIT.md 的 PRE 候选项，当前只能视为待验证假设，不能直接定性为已确认漏洞：

| 优先级 | 候选项 | 当前状态 |
| --- | --- | --- |
| 高 | 登录/Argon2 并发限制、恢复码并发消费、旧凭据验证与会话签发竞态（PRE-01/04/05/20） | 恢复码、TOTP 重放和旧凭据签发已加固；入口失败阈值并发及完整竞态仍待动态验证 |
| 高 | 文件夹锁后代目录的删除/移动/复制、Linux 路径别名和链接竞态（PRE-06/07/08） | 待动态验证 |
| 高 | S3 凭据轮换、配置备份和旧明文迁移的崩溃窗口（PRE-12/13） | 待动态验证 |
| 高 | 不同 `storage_id` 指向相同或重叠本地/S3 命名空间（PRE-25） | 待动态验证 |
| 中高 | 归档票据、上传批次票据是否绑定发起主体/会话（PRE-09/10） | 已绑定具体 Session/Gate；专项回归证明错误主体拒绝且归档票据单次使用，待真实跨账号和撤权 API 复测 |
| 中高 | 自定义 S3 Endpoint 的 DNS 变化、重定向、TLS/SNI 和 HTTP 明文边界（PRE-14） | 待动态验证 |
| 高/网络 | 回环或 LAN 模式下恶意 Host、Origin 和 DNS Rebinding 是否能借自动 Gate 或 Cookie 写请求越界（PRE-23/26） | 隔离真实监听器已验证恶意 Host、错误/多值代理头和缺失同源信息被拒绝，待实际反向代理、浏览器和 DNS 变化复测 |

### 4.1 冻结基线后的首批修复

- 登录成功签发与管理员、普通账号、网页密码、2FA 和文件夹锁状态变更使用同一认证转换锁；旧凭据验证完成后必须再次匹配当前配置才可签发令牌。
- 恢复码在认证转换内原子消费；同一 TOTP 计数器在单实例中只允许成功一次。多实例仍需共享状态或明确禁止横向扩容。
- 上传和归档票据绑定创建它的具体 Session/Gate；错误主体不会消耗归档票据，归档开始时重新验证下载权限、存储状态和文件夹锁。
- 请求日志只记录路径，不再记录包含上传/归档票据的查询字符串。
- 回环和 LAN 模式新增 Host 校验；自定义内网域名必须进入 `ALLOWED_HOSTS`。配置 `PUBLIC_BASE_URL`/`TRUSTED_PROXY_IPS` 时，即使后端监听回环地址也进入严格 HTTPS 代理模式。

## 5. 当前结论

- 代码质量和现有自动化回归是绿色的。
- 依赖许可证、来源、npm 公告、RustSec 公告和静态敏感信息扫描没有发现当前证据下的阻断项。
- 腾讯 COS、阿里 OSS、MinIO 与 RustFS 基础真实兼容性已通过，通用 S3 配置分支已基于 MinIO 通过；AWS S3 和未测试第三方实现不在通过范围内。其他 P0/P1 动态安全用例尚未取得完整证据。
- 安全加固代码基线已经冻结，但动态证据仍不完整，不能发布 Critical/High 为零或 Release 通过的结论。

## 6. 下一步建议

1. 保留 `4fa46f69c070` 作为原始功能预审起点，以 `7994d40d4fe5` 作为安全复测基线、`23b4bff998d2` 作为容器部署基线；正式发布时记录 Linux 二进制或容器镜像 Digest。
2. 在 Linux Docker/Compose 主机运行新容器门禁，保存 Compose 解析、镜像构建、非 root/只读运行、卷权限、健康检查和镜像 Digest 证据。
3. 保留已通过的 OSS、COS、MinIO、RustFS 和通用配置分支脱敏证据；继续执行大文件 Multipart、限流、网络中断、凭据轮换和恢复测试。
4. 继续执行适用的 T-01 至 T-41，优先完成真实账号权限矩阵、认证竞态、Linux 路径 TOCTOU、预览隔离和实际反向代理/DNS 用例。
5. 完成主密钥、配置和数据卷的成套备份恢复演练，并验证外部 Secret 丢失时明确拒绝而非静默重置。
6. 对确认的 Critical/High 修复后由非原实现者复测，再更新 SECURITY_AUDIT.md 的执行记录和发现项总表。
