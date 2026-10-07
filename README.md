# Ycloud

Rust + Vue 构建的自托管文件管理服务，支持本地存储和 S3 兼容对象存储。

## 功能

- 本地存储、阿里云 OSS、腾讯云 COS、MinIO、RustFS 和通用 S3。
- 上传、下载、预览、搜索、移动、复制、重命名、删除、ZIP 归档和图片画廊。
- 管理员、访客、普通用户和 WebDAV 独立授权。
- 文件夹锁、TOTP、登录限制、访问日志和传输限制。
- S3 浏览器直传、流式中转、本地原子写入和异常上传自动恢复。

网页上传同名文件自动编号另存，不覆盖原文件；WebDAV 保持客户端写入语义。普通用户由管理员创建，通过用户图标登录；盾牌为管理员入口，管理员账号只保留一个有效登录会话。

详细实现、恢复机制与维护边界见 [后台设计与维护](docs/backend-design.md)。

## 界面

<table>
  <tr>
    <th>访客入口</th>
    <th>文件管理</th>
  </tr>
  <tr>
    <td><img src="docs/images/web-login.png" alt="Ycloud 访客入口"></td>
    <td><img src="docs/images/file-browser.png" alt="Ycloud 文件管理"></td>
  </tr>
  <tr>
    <th>普通用户登录</th>
    <th>管理员登录</th>
  </tr>
  <tr>
    <td><img src="docs/images/user-login.png" alt="Ycloud 普通用户登录"></td>
    <td><img src="docs/images/admin-login.png" alt="Ycloud 管理员登录"></td>
  </tr>
</table>

管理后台概览：

![Ycloud 管理后台概览](docs/images/admin-dashboard.png)

## 开发环境与编译

- Rust 与 Cargo：`rust-toolchain.toml` 固定 1.96.1；需安装对应平台的原生编译工具链。
- Node.js：`^20.19.0` 或 `>=22.12.0`；npm：11.17.0。前端依赖以 `frontend/package-lock.json` 为准。
- Docker 部署另需 Docker Engine 和 Compose 插件。

从仓库根目录先构建前端，再编译后端；Rust 会将 `static/app/` 中的前端产物嵌入可执行文件：

```bash
cd frontend
npm ci
npm run build
cd ..
cargo build --release --locked
```

可执行文件位于 `target/release/ycloud`（Windows 为 `ycloud.exe`）。修改前端后需重新构建，并提交完整的 `static/app/`（含分包及自动生成的 `embedded-assets.rs`），不能只复制入口 JS。

开发时在仓库根目录运行 `cargo run --locked`，另开终端在 `frontend/` 运行 `npm run dev`，访问 `http://127.0.0.1:5173`。Vite 将 API 请求转发到默认的后端地址 `127.0.0.1:18473`。首次启动生成的凭据保存在运行目录的 `initial-credentials.json`，请勿提交。

## Docker Compose

```bash
mkdir -p data
sudo chmod 700 data
docker compose up -d --build
docker compose ps
```

首次部署只初始化账号和配置，不自动添加存储。登录后台后自行添加本地或 S3 存储；删除存储配置不会删除文件。文件页按权限展示存储，并按账号记住最后手动选择的存储。

访问 `http://服务器IP:18473`。读取首次启动凭据：

```bash
sudo cat ./data/initial-credentials.json
```

修改管理员密码和网页访问密码后，该文件自动删除。`./data` 包含配置、主密钥、本地文件和运行状态，备份时整体处理。

容器默认以 root 运行，不强制指定 UID/GID。建议按需在 Compose 中启用 `user: "UID:GID"`，并确保该账号可读写整个数据目录；例如使用 `1000:1000` 时，新建空目录可执行 `sudo chown 1000:1000 data`。已有数据应先备份，再核对文件归属及 NAS ACL，避免盲目递归修改权限。

停止等待时间由用户选择：Compose 中可启用 `stop_grace_period: 60s`；大文件传输场景可考虑 `5m`，最好在传输结束后再停服。不设置时 Docker 默认等待 10 秒，超时会强制终止；延长等待不保证所有任务都能完成。

Compose 默认轮转容器标准输出日志，每份 `10m`、最多 3 份，约 30 MB；这不限制数据目录中的业务访问日志。修改配置后运行 `docker compose up -d --build --force-recreate`，已有容器需重建才能应用日志配置。

### Docker 命令部署

从仓库根目录构建并启动：

```bash
docker build --tag ycloud:local .
mkdir -p data
sudo chmod 700 data
docker run -d --name ycloud --restart unless-stopped \
  -p 18473:18473 \
  --mount type=bind,src="$(pwd)/data",dst=/var/lib/ycloud \
  --log-driver json-file --log-opt max-size=10m --log-opt max-file=3 \
  ycloud:local
```

可选：在镜像名之前添加 `--user UID:GID`、`--stop-timeout 60`，分别选择运行账号和停止等待秒数。数据目录与首次凭据读取方式同 Compose。

## Linux 原生运行

完成上方编译后，从仓库根目录运行：

```bash
BIND_ADDRESS=0.0.0.0 ./target/release/ycloud
```

默认配置和存储目录为当前目录下的 `config.json`、`storage/`。

## 部署配置

| 变量 | 用途 |
| --- | --- |
| `BIND_ADDRESS` | 监听地址；原生运行默认 `127.0.0.1` |
| `PORT` | HTTP 端口，默认 `18473` |
| `CONFIG_PATH` | 配置文件路径 |
| `STORAGE_PATH` | 可供管理员添加的本地挂载路径 |
| `ALLOWED_HOSTS` | 允许的内网 Host，多个值用逗号分隔 |
| `LOCAL_STORAGE_MOUNTS` | 后台可选本地挂载点的 JSON 数组 |
| `RUST_LOG` | 日志级别 |

Dockerfile 已设置容器内的监听地址、端口和数据路径，普通 Compose 部署无需设置访问模式参数。

反代部署若要记录真实访客 IP，在后台“域名绑定”的访问地址下填写“可信代理 IP”，确认后立即生效并随配置保存，无需环境变量或重启。填写 Ycloud 实际看到的代理连接源 IP，不带协议或端口；多个 IPv4/IPv6 用逗号分隔，留空不信任转发头。代理须覆盖 `X-Real-IP`，Nginx Proxy Manager 默认提供此头；旧日志不改写，反代部署建议仅让代理访问后端端口。

### 配置主密钥

单机自动生成主密钥并保存在数据目录。容器部署或使用外部密钥管理时可设置 `YCLOUD_CONFIG_KEY_FILE`，也可用 `YCLOUD_CONFIG_KEY` 覆盖。密钥为 32 个随机字节的 URL-safe Base64；密钥丢失后，已加密的 S3 SecretKey 和 TOTP 密钥无法恢复。

### 存储

在线更换连接、收紧权限或停用存储可能中断当前传输。未完成上传由后台自动处理；结果“待确认”时不要重复上传。删除存储配置不会删除文件，但须先移除 WebDAV、文件夹锁和账号权限引用。

“允许访客访问”只控制无账号的网页访问；WebDAV 使用独立的挂载凭据和读写权限。

WebDAV 共用后台单文件上传大小限制、全站流量额度和上传/下载全局限速（0 表示不限速）；本地支持未知长度流式上传，下载支持单段 Range，上传不支持断点续写。成功密码验证使用有界短时内存缓存，后续请求仍检查当前挂载与权限；超时不证明文件未提交，重传前应先核对目标文件。

在线修改全局限速也作用于已有传输后续放行的数据；取消限速会唤醒等待者，已放行的数据不会被追回。不限速不等于免除流量额度。

文件下载及 WebDAV 文件读取统一返回附件响应和内容隔离响应头，文件原始字节不变；直接用浏览器访问 WebDAV 文件地址会下载文件。预览使用独立的类型策略，不由 S3 对象的 Content-Type 元数据决定是否可内联展示。

HEAD 只返回文件元数据，不读取正文。条件续传（`If-Range`）只有匹配当前 S3 强 ETag 时返回分段；版本不同或无法证明一致时返回完整文件。本地文件不以大小、时间冒充强版本，因此普通 Range 可用，但带 `If-Range` 时保守返回完整文件。

WebDAV 目录查询支持 `Depth: 0` 和 `Depth: 1`、指定属性、属性名及 allprop/include。目录递归查询（含省略 Depth）明确返回 403 和 `propfind-finite-depth`，不会只列一层却假报递归成功；客户端应使用 Depth 0/1。当前不支持的属性单独返回 404，未增加 WebDAV 锁或断点续写能力。

WebDAV PUT 支持写入条件，条件失败返回 412 且不改动目标。本地支持存在性条件及修改时间核验，不提供虚假的强 ETag；S3 核验原子条件能力后使用版本条件发布，不确定结果不自动重传或回滚正式文件。其他方法的写入条件、DAV If 锁条件、OSS 条件上传及超过 4 GiB 的 S3 条件上传目前明确拒绝，不忽略条件继续写入；无条件上传保持原有规则。详细边界见 [后台设计](docs/backend-design.md#webdav-条件写入边界)。

额外挂载点示例：

```env
LOCAL_STORAGE_MOUNTS=[{"id":"archive","name":"Archive","path":"/srv/ycloud/archive"}]
```

自建 S3（MinIO、RustFS 等）直接在管理员后台填写 Endpoint、Bucket 和访问密钥，无需设置地址白名单。Endpoint 只接受不含凭据、路径或查询参数的 HTTP(S) 地址；内网可使用 HTTP，非可信网络请使用 HTTPS。

S3 直传需要浏览器能够访问所填写的 Endpoint，且存储桶的 CORS 允许 Ycloud 网页来源发起 `PUT` 请求（允许请求头 `*`）。HTTPS 网页需要 HTTPS 存储地址，不能使用被浏览器拦截的 HTTP 混合内容。密钥不交给浏览器；后台只签发绑定临时对象、分片和有效期的上传地址，并核验分片后发布最终文件。

浏览器无法直传、但 Ycloud 服务器能够访问的存储，可以开启“中转上传”，不限于内网存储。升级时已有 S3 配置保留中转模式，新建配置默认直传；直传失败不会自动改用中转。S3 直传不经过 Ycloud 的实时限速，不计入任何流量统计或额度，管理员、普通用户及访客均按相同传输路径规则处理；权限、文件大小及存储容量限制仍生效。中转及 WebDAV 仍按经过服务器的文件数据计量。当前 S3 下载由服务器转发，仍需计量，不能仅因存储类型为 S3 就排除；真正的直链跳转下载不计量。

阿里云 OSS 和腾讯云 COS 使用与 Region 对应的官方 HTTPS Endpoint。同一 Bucket/Prefix 只允许一个 Ycloud 实例写入。

建议在存储桶控制台配置“未完成分片上传”的生命周期清理规则，以回收异常断连留下的空会话；Ycloud 不自动修改桶规则。

### 传输上限

后台可调整业务上限；以下变量定义后台不可突破的部署上限：

首次启动的业务默认值自动取部署上限与内置默认值中的较小值；已有配置不自动降低。上传和归档字节上限至少为 1 MiB，批次字节上限不得小于单文件上传上限。

- `MAX_UPLOAD_BYTES`
- `MAX_UPLOAD_BATCH_BYTES`
- `MAX_UPLOAD_BATCH_ENTRIES`
- `MAX_ARCHIVE_BYTES`
- `MAX_ARCHIVE_ENTRIES`

## 检查

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --locked

cd frontend
npm run check
```

## 流量额度与统计

- 限额：全站、访客共享下载、普通用户共享和账号独立额度；上传／下载分开计算，`0` 表示不限额。
- 统计：仪表盘只计经 VPS 的文件传输，用户上传为入站、用户下载为出站；S3 直传及直链跳转下载不计入任何角色的统计或限额。经服务器的下载、预览和打包仍计量。关闭限额仍计量；管理员中转传输计量但不限额，独立 WebDAV 受全站限额约束。这是应用层文件数据统计，不是网卡总流量，不包含系统/API 开销或 S3 中转的后端连接额外流量。
- 历史数据：旧版本没有记录直传／中转类别，无法可靠扣除旧直传量；升级保留现有数据，新规则从升级后生效，按原周期重置。
- 重置与备份：按小时／天／月周期重置，重启不清零。停服备份时需将配置与 `traffic-usage.json`、`traffic-usage.jsonl` 一同保存。

### 账本故障排查

流量账本损坏时不会清零额度或绕过限制。停服并备份账本后，在相同 `CONFIG_PATH` 环境运行 `ycloud doctor traffic` 进行只读检查；该命令不修复、不创建文件。完整记录损坏或序号缺口须从可信备份恢复，不要直接删除账本重来。

## 开源组件

Ycloud 使用 Vue、Phosphor Icons、Reka UI、Tokio、Axum、AWS SDK for Rust 等开源组件；项目名单和必要许可文本见 [第三方声明](THIRD_PARTY_NOTICES.md)。
