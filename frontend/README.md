# Ycloud Web

这里是 Ycloud 正式使用的 Vue 3.5 + TypeScript 前端源码。首页、文件浏览、文件预览和五个管理页面由同一应用提供；生产构建写入 `../static/app/`，随后由 Rust 编译进单个可执行文件。

界面内置简体中文和英文，首次访问默认使用简体中文，用户可在各正式页面的顶栏切换。选择结果保存在浏览器本地的 `ycloud-locale`，只记录语言偏好，不包含身份或文件信息；切换时同步更新页面的 `lang` 属性。翻译层位于 `src/shared/i18n/`，不依赖外部服务，也不增加运行时网络请求。

正式路由：

- `/`：网页访问密码入口。
- `/browse`：文件浏览与管理员文件操作。
- `/preview`：安全类型白名单内的文件预览。
- `/admin/account`、`/admin/security`、`/admin/limits`、`/admin/locks`、`/admin/webdav`：管理设置。

前端只调用同源 `/api/*`，认证继续使用后端签发的 `HttpOnly` Cookie。真实密码、哈希和会话令牌不会写入浏览器存储。文件权限、路径边界、容量上限、文件夹锁与 WebDAV 冲突校验都以 Rust 后端为最终判断。

## 检查与生产构建

```powershell
cd frontend
npm ci
npm run check
```

`npm run check` 依次执行 ESLint、Vitest、Vue/TypeScript 类型检查和 Vite 生产构建。输出固定为：

```text
static/app/index.html
static/app/assets/app.css
static/app/assets/app.js
```

固定文件名避免 Rust 构建时解析动态清单；响应使用重新验证缓存策略，部署新可执行文件后不会继续引用旧哈希资源。生产运行只需仓库根目录的 `cargo run --release --locked`，不启动 Vite。

## 本地热更新

需要调整界面时，先保持 Rust 服务运行，再开启开发服务器：

```powershell
npm run dev
```

开发入口为 `http://127.0.0.1:5173/v2/`。`/v2` 只用于 Vite 开发环境；生产链接使用正式路由，旧候选书签由 Rust 永久重定向。

依赖全部精确锁定在 `package-lock.json`。更新时按运行依赖、构建工具、测试工具分组验证，不在没有测试和构建结果时批量升级。
