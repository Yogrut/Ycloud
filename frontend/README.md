# Ycloud 前端

Vue 3.5 + TypeScript + Vite。生产产物写入 `../static/app/` 并嵌入 Rust 可执行文件。

## 安装与检查

```bash
npm ci
npm run check
```

`npm run check` 执行 ESLint、Vitest、TypeScript 检查和生产构建。

## 开发模式

终端 1（仓库根目录）：

```bash
cargo run --locked
```

终端 2（仓库根目录）：

```bash
cd frontend
npm run dev
```

## 目录

```text
src/apps/               页面和业务组件
src/apps/browser/       文件页组件及 useBrowser* 页面业务逻辑
src/components/ui/      可直接维护的基础 UI 组件源码
src/lib/                基础 UI 组件使用的通用工具
src/shared/api/         API 客户端
src/shared/components/  共享组件
src/shared/composables/ 共享状态逻辑
src/shared/i18n/        中英文资源
src/shared/styles/      主题和布局
```

## 规则

- 图标使用 `@phosphor-icons/vue`。
- 不引入远程字体、远程脚本或第二套图标系统。
- 页面专用状态和操作逻辑保留在对应的 `src/apps/<app>/` 目录，可复用逻辑再放入 `src/shared/`。
- 基础 UI 组件放在 `src/components/ui/`，业务组件不放入该目录。
- 权限由后端判断；前端只控制交互显示。
- API 调用通过 `src/shared/api/`。
- 修改前端后提交对应的 `static/app` 构建产物。
