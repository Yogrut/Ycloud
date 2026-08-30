# Ycloud 安全审查执行报告

> 本报告对应 2026-08-30 在当前工作区执行的预审。它不是安全认证，也不是 Release 签字。由于工作区存在未提交改动、缺少隔离动态环境和部分在线公告数据，本报告不作“通过”结论。

## 1. 审查基线

| 项目 | 结果 |
| --- | --- |
| 分支 | `main` |
| HEAD | `0961e39578f8fc8c6a68549234934da2a4c40694` |
| 工作区 | 不干净；存在既有未提交功能、前端和安全改动 |
| `git diff --check` | 通过；仅报告 Git 的 LF/CRLF 提示 |
| Rust | `rustc 1.96.1` / `cargo 1.96.1` |
| Node/npm | `node v24.19.0` / `npm 11.17.0` |
| Windows Release SHA-256 | `D3BD08770FE050B51C80D3BE9CEB5B77286259233F005F37B71F0B3A9235FFFD` |
| 前端 JS SHA-256 | `131E1559155F25AC823FA580575C605490B97350D67948A3274B08048868E6E5` |
| 前端 CSS SHA-256 | `A7E01BE90158E0811983076F149412C1789F543CC12E643320101E1CA7921342` |

## 2. 已执行检查

| 检查 | 结果 | 说明 |
| --- | --- | --- |
| `cargo fmt --all -- --check` | 通过 | 无格式差异 |
| `cargo clippy --locked --all-targets -- -D warnings` | 通过 | 无 Clippy 警告 |
| `cargo test --locked` | 通过 | 102 通过、0 失败、1 ignored |
| `cargo build --locked --release` | 通过 | 生成 Windows Release 可执行文件 |
| `npm ci` | 通过 | 依照 lockfile 安装 |
| `npm run check` | 通过 | lint、27 个测试文件/110 项测试、typecheck、build 均通过 |
| `cargo deny check bans licenses sources` | 通过 | 许可证、来源和禁用依赖通过；存在重复依赖警告 |
| `npm audit --audit-level=moderate` | 通过 | 0 vulnerabilities |
| 静态敏感信息扫描 | 未发现匹配 | 扫描了提交文件中的常见 Access Key、私钥和明文 Secret 模式；不替代 Git 历史专用扫描器 |

## 3. 未完成或受阻检查

### 3.1 RustSec 公告数据库

`cargo deny check` 和 `cargo audit` 均尝试访问 RustSec/GitHub，但当前环境无法完成 GitHub 公告数据库拉取。许可证、来源和禁用依赖检查已单独通过；不能据此声称 RustSec 漏洞检查通过。

### 3.2 S3 真实环境

全量 Rust 测试中的 `s3_backend::tests::s3_compatibility_smoke` 按设计为 ignored，需要隔离 Bucket/Prefix 和临时凭据。本次环境没有 `YCLOUD_S3_SMOKE_*` 测试变量，因此没有对阿里 OSS、腾讯 COS、MinIO/RustFS 或目标通用 S3 执行真实网络读写验收。

代码已有 S3 测试连接探测：列举、随机内部前缀写入、HEAD/ETag、读取、服务端复制和删除，并在失败时清理临时对象。该探测不等于完整的大文件、Multipart、故障恢复和厂商兼容性验收。

### 3.3 动态安全矩阵

SECURITY_AUDIT.md 中 T-01 至 T-41 的隔离动态用例本次未全部执行，原因是没有专用管理员/普通账号、隔离本地目录、四类 S3、WebDAV 客户端、反向代理、公网 HTTPS 和故障注入环境。涉及认证竞态、CSRF/Host/DNS Rebinding、Windows ACL、票据交换、路径 TOCTOU、预览隔离和恢复演练的结论均应保持“待动态验证”。

## 4. 需要优先验证的静态候选项

以下来自 SECURITY_AUDIT.md 的 PRE 候选项，当前只能视为待验证假设，不能直接定性为已确认漏洞：

| 优先级 | 候选项 | 当前状态 |
| --- | --- | --- |
| 高 | 登录/Argon2 并发限制、恢复码并发消费、旧凭据验证与会话签发竞态（PRE-01/04/05/20） | 待动态验证 |
| 高 | 文件夹锁后代目录的删除/移动/复制、Windows 路径别名和链接竞态（PRE-06/07/08） | 待动态验证 |
| 高 | S3 凭据轮换、配置备份和旧明文迁移的崩溃窗口（PRE-12/13） | 待动态验证 |
| 高 | 不同 `storage_id` 指向相同或重叠本地/S3 命名空间（PRE-25） | 待动态验证 |
| 高/平台 | Windows 服务目录 ACL 是否阻止低权限账号读取或修改配置、主密钥、备份和日志（PRE-11） | 待 Windows 动态验证 |
| 中高 | 归档票据、上传批次票据是否绑定发起主体/会话（PRE-09/10） | 待动态验证 |
| 中高 | 自定义 S3 Endpoint 的 DNS 变化、重定向、TLS/SNI 和 HTTP 明文边界（PRE-14） | 待动态验证 |
| 高/网络 | 回环或 LAN 模式下恶意 Host、Origin 和 DNS Rebinding 是否能借自动 Gate 或 Cookie 写请求越界（PRE-23/26） | 待动态验证 |

## 5. 当前结论

- 代码质量和现有自动化回归是绿色的。
- 依赖许可证、来源、npm 公告和静态敏感信息扫描没有发现当前证据下的阻断项。
- RustSec 在线公告检查、真实 S3 兼容性和大部分 P0/P1 动态安全用例尚未取得证据。
- 当前工作区不是冻结基线，不能发布 Critical/High 为零或 Release 通过的结论。

## 6. 下一步建议

1. 冻结审核提交，保存补丁哈希和最终产物哈希。
2. 在隔离环境执行 T-01 至 T-41，优先认证、会话、权限、路径和票据用例。
3. 为 OSS、COS、MinIO/RustFS、目标通用 S3 各准备一次性账号、Bucket 和 Prefix，运行 ignored S3 smoke。
4. 在可联网审核机重新执行 `cargo audit`、`cargo deny check`，记录公告数据库日期。
5. 完成 Windows ACL、反向代理/DNS、主密钥恢复和备份恢复演练。
6. 对确认的 Critical/High 修复后由非原实现者复测，再更新 SECURITY_AUDIT.md 的执行记录和发现项总表。
