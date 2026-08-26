# Ycloud-v2 任务交接

本文只描述当前工作树、有效边界和下一步，不记录旧版本维护流水。

## 当前状态

- 工作目录：`D:\SYStemFiles\文档\open_code\Ycloud-v2`
- 当前分支：`main`
- 当前提交：`abe133d5d3741d25c5071a84c576b277af3de4c2`
- 远端：`origin = https://github.com/Yogrut/Ycloud.git`
- 工作树包含一整批尚未提交的后端、前端、文档和生产静态资源改动。不要执行 `git reset --hard`、`git checkout -- .`、清理未跟踪文件或用旧提交覆盖当前工作树。
- 当前改动跨越多存储、S3、用户账号、按存储授权、管理后台和文件浏览器，不能拆掉其中一层后假定其他层仍然完整。

## 固定目标与取舍

- 权重：安全 40%、性能 30%、稳定 30%。数据安全性与数据完整性并列优先。
- 目标部署：单进程、1 vCPU、低内存 VPS；文件上传、下载、预览和 ZIP 必须流式处理。
- 保留 Rust、Tokio、Axum 0.8、Vue 3.5、TypeScript 和 Vite；生产运行不依赖 Node、数据库或外部 CDN。
- 不引入完整多用户、用户组、角色继承或文件级 ACL。管理员是唯一后台管理身份；用户账号只能由管理员创建、改密、停用、删除和授权。
- 不把多个存储合并成虚拟根目录。每次请求使用 `storage_id + path`，路径栏仍只显示 `/目录/`。
- 不允许跨存储复制或移动；不得把本地文件系统的原子重命名假设套用到 S3。
- 未经用户明确要求，不提交、不推送、不删除真实存储数据，也不迁移现有用户文件。

## 当前实现

### 前端与运行方式

- `frontend/` 是唯一前端源码，使用 Vue 3.5.41、TypeScript 6.0.3、Vite 8.2.2。
- Vite 构建产物写入 `static/app/` 并嵌入 Rust 可执行文件。生产只启动 Rust；Vite 双终端只用于前端开发热更新。
- 文件浏览、预览、登录和管理后台已经迁入 Vue；支持中英文、亮暗主题、桌面框选、移动端选择面板和顶栏存储切换。
- 当前视觉语言为 Clean Blue。所有语义图标经共享 `AppIcon.vue` 使用 `@phosphor-icons/vue` 的 Duotone 风格和项目蓝色；不要再切换到 Material Symbols，也不要在没有产品要求时给文字按钮擅自添加图标。
- 管理后台使用顶部 Ycloud 品牌栏、下方连续单列导航和右侧内容区。导航名称已经确定为：存储设置、WebDAV、文件夹锁、传输限制、管理员设置、用户管理、登录保护、访问日志；不再显示导航分组小标题。
- 文件页把搜索、新建文件夹、上传、路径和文件列表放在同一文件面板内；桌面和手机端均使用紧凑文件行。列表使用 10/20/50/100 条服务端分页和上一页/下一页游标，右下角旧“共 N 项”已移除。文件行没有三点菜单，桌面操作通过右键菜单，移动端通过选择后的操作面板。
- 文件页顶栏的存储切换器只显示存储名称，不显示图标或后台运行状态；切换后重置路径、搜索、分页和选择。根路径只显示 Home 图标，不显示 `Home` 文字。
- 管理后台“新建存储”和存储列表“设置”是纯文字按钮，与“新建挂载”“新建锁”保持一致；不要重新添加图标。存储编辑弹窗使用 `min(1120px, 100vw - 48px)` 的桌面宽度，移动端保持视口内滚动。

### 存储层

- `schema_version: 9`，最多 16 个存储实例、一个待添加实例和 100 个用户账号；每个存储源独立保存启停与访客访问策略。本地存储表单直接填写路径，后端再将它解析到 `STORAGE_PATH`/`LOCAL_STORAGE_MOUNTS` 已声明的唯一部署挂载点，新增和设置使用同一套名称、路径与容量接口。
- 已有本地存储可重新配置名称、路径、容量上限、启停和访客访问；路径变化时后端先准备新运行时存储，配置持久化成功后再替换注册表中的后端并清空归档票据。路径必须已挂载到 Ycloud 进程或容器且由部署配置声明，后台不再显示挂载点下拉框。
- 后台每个存储源只呈现三个互斥运行状态：启用、停用、异常；“允许访客访问”是独立策略，不是第四种状态。运行状态只在后台显示，文件页只提供存储切换。
- 不向用户提供默认存储或“设为默认”。内部 `default_storage_id` 暂时只为旧配置和旧管理请求兼容，不参与文件页选源；不要在没有完整迁移方案时直接删除该字段。
- 删除存储只移除 Ycloud 配置与运行时连接，不删除本地目录、Bucket、Prefix 或其中的文件。若仍被 WebDAV、文件夹锁或用户权限引用，后端拒绝删除，不能级联清除授权关系。
- 本地文件系统与 S3 共用 `storage_backend.rs` 路径级接口，由运行时注册表按不可变 `storage_id` 路由。
- `storage_catalog.rs` 管理部署声明的本地挂载白名单；每个本地实例拥有独立服务与命名空间，额外目录必须预先存在，所有本地实例共享一个有界 I/O 门限。
- S3 共享一套 AWS SigV4 引擎，提供阿里云 OSS、腾讯云 COS、MinIO/RustFS 和通用 S3 预设。
- 已实现受限 Endpoint、连接探测、两阶段添加、流式上传下载、Range、容量跟踪、条件写入、文件与目录复制/移动/删除、事务恢复和流式 ZIP。
- RustFS `1.0.0-beta.12` 与腾讯云 COS 已通过独立测试桶的真实兼容性冒烟测试。AWS、阿里云 OSS、独立 MinIO 仍需真实服务验证。
- S3 目录事务一次最多 1000 个对象；当前单次 `CopyObject` 使含超过 5 GiB 对象的目录事务被拒绝，尚未实现分段复制。

### 认证与权限

- 管理员 Cookie 会话拥有后台及全部存储权限；管理 API 不接受管理员 Basic Auth。
- 通过网页访问密码进入的访客，只能浏览、预览和下载管理员明确允许访客访问的已启用存储源。
- 用户账号拥有独立密码、登录限流和 Cookie 会话，不能进入管理后台。
- 管理员可按存储授予用户账号：浏览、下载、上传、新建目录、重命名、移动、复制、删除。
- 后端在每个请求上强制校验权限；前端隐藏按钮不是安全边界。
- 用户账号停用、删除、改密或权限变化后撤销对应会话。
- WebDAV 使用独立挂载 Basic Auth，绑定单个 `storage_id + path`，不继承用户账号权限。
- 文件页列出所有已添加、已启用且运行可用的存储源。访客选择不允许访客访问的存储源时先打开账号登录，不得在认证前读取目标存储；管理员和获得该存储浏览权限的用户账号登录后可以进入。

### 资源与安全边界

- 默认监听 `127.0.0.1:18473`。局域网明文访问必须显式设置 `BIND_ADDRESS=0.0.0.0` 与 `ALLOW_LAN_HTTP=true`；公网模式要求可信 HTTPS 反向代理、固定公共来源、安全 Cookie 和可信代理 IP。
- S3 Endpoint 是 SSRF 边界。MinIO、RustFS 和通用 S3 必须精确匹配 `S3_ALLOWED_ENDPOINTS`；云厂商预设只接受官方 HTTPS Endpoint。
- 凭据不得写入仓库、日志、错误详情或管理 API 响应。真实测试密钥只能通过当前进程环境变量注入，并在测试后撤销。
- 文件 I/O、WebDAV、Argon2、归档、S3 请求、事务和磁盘安全余量保持固定上限，不开放给管理员随意调整。
- AWS SDK 1.143.0 间接依赖的 `lru 0.16.4` 命中 `RUSTSEC-2026-0253`；Ycloud 已关闭仅受影响的 S3 Express 会话认证路径，`deny.toml` 仅保留该编号的临时例外。上游允许安全版本后必须移除例外。

## 2026-08-26 当前快照验证

- `cargo fmt --all`：已执行。
- `cargo check`：通过。
- `cargo test`：83 通过、0 失败、1 个真实 S3 冒烟测试按设计忽略，共 84 项。
- `frontend\npm run typecheck`：通过。
- `frontend\npm run test`：22 个测试文件、88 项测试全部通过。
- `frontend\npm run lint`：通过，0 warning。
- `frontend\npm run build`：通过，生产资源已更新到 `static/app/`。
- `git diff --check`：通过；仅提示 Windows 工作树未来可能执行 LF 到 CRLF 转换。
- 最新存储 UI 改动后尚未重新运行 `cargo clippy --locked --all-targets -- -D warnings` 和 `cargo build --release --locked`，发布或提交前应补跑。
- Codex 浏览器视觉检查未完成：Windows 沙箱辅助进程持续以 `helper_unknown_error: setup refresh had errors` 退出。自动化 DOM、类型、Lint、Rust 和构建检查全部通过，但下一位接手者仍应启动真实页面做桌面与手机视觉验收。

## 当前工作树注意事项

- `static/app/assets/app.css` 和 `static/app/assets/app.js` 是本次前端检查重新生成的生产资源，应与对应 Vue/TypeScript 源码一起评估。
- 相对当前提交，工作树约有 42 个已跟踪文件变化（约 1700 行新增、892 行删除），另有未跟踪文件；这是一组相互关联的多存储、权限和 UI 改造，尚未提交。
- `frontend/src/shared/components/icons/CloudIcon.vue` 已删除；云图标和其他图标统一由新的 `frontend/src/shared/components/AppIcon.vue` 提供。不要恢复旧的单用途图标文件。
- 当前未跟踪内容包括 `.obsidian/`、`CONTEXT.md`、`ProtectionView.vue` 及其测试、`FileIcon.test.ts`、`AppIcon.vue`、`LocaleToggle.test.ts` 和 `icon-system.test.js`。其中 `.obsidian/` 不是项目运行必需内容，提交前必须由用户决定是否保留或忽略，不要擅自删除或提交。
- `CONTEXT.md` 定义管理员、用户账号、登录保护、访问日志、存储源、存储源状态和访客访问等项目统一语言；后续改名或领域设计先更新该文档，避免界面与代码术语再次分裂。
- `.agents/skills/` 与 `skills-lock.json` 是项目本地安装的技能；先阅读其 `SKILL.md` 再决定是否用于任务，不要盲目执行第三方指令。
- 当前仓库没有根级 `AGENTS.md`。如果以后使用 `setup-matt-pocock-skills` 生成项目说明，必须先向用户展示草案并获得确认。

## 下一步

1. 先执行 `git status --short --branch`，完整阅读本文件、`CONTEXT.md`、`ARCHITECTURE.md` 与 `STORAGE_BACKEND_PLAN.md`，不要重复已经完成的架构改造，也不要覆盖当前脏工作树。
2. 启动真实页面做视觉验收：确认文件页存储切换器无左侧图标，“新建存储”和“设置”无图标，存储编辑弹窗桌面端明显加宽，手机端仍可完整滚动。
3. 在部署配置中声明两个真实本地挂载目录，验证新建本地存储可手填路径；再验证已有存储可修改名称、切换到另一未占用挂载地址、修改容量与访问策略，并确认原目录文件没有被移动或删除。
4. 对用户账号做浏览器级冒烟测试：创建账号、登录、单存储与多存储切换、八类权限逐项允许/拒绝、权限收回与会话撤销、禁止访问 `/admin`。
5. 对管理员、首页访客、用户账号和 WebDAV 做权限回归，重点验证构造 `storage_id`、批量接口、ZIP、预览和 WebDAV 方法不能越权。
6. 补跑 Clippy 与 release 构建；随后检查 `.obsidian/` 和所有未跟踪源码，向用户报告拟提交清单，未经确认不要删除、提交或推送。
7. 权限回归通过后，再继续 S3 故障注入，以及 AWS、阿里云 OSS、MinIO 的隔离测试桶兼容验证。任何真实 S3 测试必须使用独立 Bucket/Prefix 和最小权限临时凭据；不得使用生产 Bucket、根账号或用户已在对话中暴露过的旧密钥。

## 启动与检查

生产式单终端启动：

```powershell
cd D:\SYStemFiles\文档\open_code\Ycloud-v2
cargo run --release --locked
```

默认仅监听 `127.0.0.1:18473`。可信局域网需要明文直连时使用：

```powershell
cd D:\SYStemFiles\文档\open_code\Ycloud-v2
$env:BIND_ADDRESS = "0.0.0.0"
$env:PORT = "18473"
$env:ALLOW_LAN_HTTP = "true"
$env:SECURE_COOKIES = "false"
cargo run --release --locked
```

公网部署必须由同机可信 HTTPS 反向代理访问。反向代理连接本机回环地址时，保持默认监听并使用：

```powershell
cd D:\SYStemFiles\文档\open_code\Ycloud-v2
$env:SECURE_COOKIES = "true"
$env:PUBLIC_BASE_URL = "https://cloud.example.com"
$env:TRUSTED_PROXY_IPS = "127.0.0.1"
cargo run --release --locked
```

将 `PUBLIC_BASE_URL` 替换为实际 HTTPS 来源，将 `TRUSTED_PROXY_IPS` 设置为代理连接 Ycloud 时使用的精确 IP。代理若不通过回环地址连接，还需按部署网络设置 `BIND_ADDRESS=0.0.0.0`；不要同时开启 `ALLOW_LAN_HTTP`，也不要把这种配置当成公网 HTTP 直连方案。代理必须覆盖为单一 `X-Forwarded-For`，并发送 `X-Forwarded-Proto: https`。

前端热更新开发才需要两个终端：根目录运行 Rust，`frontend/` 运行 `npm run dev`。

完整本地检查：

```powershell
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
cd frontend
npm run check
```

## 必读文档

- `CONTEXT.md`：身份、权限与存储领域的统一术语。
- `ARCHITECTURE.md`：当前实现与安全边界。
- `STORAGE_BACKEND_PLAN.md`：多存储/S3 已完成能力、限制与真实兼容测试方法。
- `README.md`：部署、环境变量和日常运行。
- `frontend/README.md`：前端开发与生产资源构建方式。
