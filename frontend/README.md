# Ycloud 前端

Vue 3.5 + TypeScript + Vite。生产产物写入 `../static/app/` 并嵌入 Rust 可执行文件。

## 安装与检查

```bash
npm ci
npm run check
```

`npm run check` 执行 ESLint、Vitest、TypeScript 检查和生产构建。

## 开发模式

终端 1：

```bash
cargo run --locked
```

终端 2：

```bash
cd frontend
npm run dev
```

## 目录

```text
src/apps/               页面和业务组件
src/shared/api/         API 客户端
src/shared/components/  共享组件
src/shared/composables/ 共享状态逻辑
src/shared/i18n/        中英文资源
src/shared/styles/      主题和布局
```

## 规则

- 图标使用 `@phosphor-icons/vue`。
- 不引入远程字体、远程脚本或第二套图标系统。
- 权限由后端判断；前端只控制交互显示。
- API 调用通过 `src/shared/api/`。
- 修改前端后提交对应的 `static/app` 构建产物。
