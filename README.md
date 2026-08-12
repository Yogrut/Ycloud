# Ycloud

Ycloud 是一个使用 Rust 和 Axum 构建的轻量级私有文件服务，提供网页文件管理、网页文件夹锁和供外部软件连接的 WebDAV。

项目长期遵循三个同等重要的目标：

> 稳定、安全、高性能，三者平衡，不以明显牺牲另外两项为代价追求单项极致。

当前版本的完整结构、权限边界、配置语义和接口说明见 [ARCHITECTURE.md](ARCHITECTURE.md)。

## 启动

项目运行只需要稳定版 Rust 工具链，不需要 Node.js。

```powershell
cd D:\SYStemFiles\文档\Ycloud
cargo run --release --locked
```

默认配置：

- 监听地址：`0.0.0.0:3000`（本机和可信局域网）
- 文件目录：`./storage`
- 配置文件：`./config.json`
- 单次上传上限：512 MiB

### 局域网与 WebDAV

默认启动已经允许手机或其他局域网设备连接。使用 `ipconfig` 查看电脑当前 IPv4 地址；路由器重新分配地址后，客户端中的旧地址需要同步更新。

WebDAV 客户端填写服务器当前的局域网 IPv4 地址、端口 `3000`、后台显示的 `/dav/挂载名称` 路径，以及该挂载独立的用户名和密码。Windows 首次询问网络访问时只允许“专用网络”。未设置网页访问密码时，无密码网页授权仅允许本机获取，局域网 WebDAV 仍使用挂载自己的 Basic 认证。只需要本机访问时，可设置 `BIND_ADDRESS=127.0.0.1`。

服务器会在未认证或认证失败时返回标准 `WWW-Authenticate: Basic` 挑战，兼容不会预先发送密码的移动端 WebDAV 客户端。客户端显示 `401` 时，应重新输入该挂载自己的用户名和密码；它们不是管理员账号或网页访问密码。

首次运行会生成随机管理员密码并输出到终端。首次登录后应立即更换管理员密码。

## 环境变量

```powershell
$env:BIND_ADDRESS = "0.0.0.0"
$env:PORT = "3000"
$env:STORAGE_PATH = ".\storage"
$env:CONFIG_PATH = ".\config.json"
$env:MAX_UPLOAD_BYTES = "536870912"
$env:IO_CONCURRENCY = "8"
$env:MAX_LIST_ENTRIES = "10000"
$env:REQUEST_TIMEOUT_SECS = "300"
$env:SECURE_COOKIES = "false"
cargo run --release --locked
```

公网部署必须放在 HTTPS 反向代理后，并设置：

```powershell
$env:SECURE_COOKIES = "true"
```

## 质量检查

```powershell
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
```

`storage`、实际 `config.json` 和 `config.json.bak` 属于运行数据，不应提交到 Git。`config.example.json` 只描述当前配置结构。
