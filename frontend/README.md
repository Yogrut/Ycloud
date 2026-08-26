# Ycloud 前端

Vue 3.5 + TypeScript 前端，使用 Vite 构建。生产产物写入 `../static/app/` 并由 Rust 服务内嵌。

## 安装与检查

```powershell
cd frontend
npm ci
npm run check
```

`npm run check` 依次执行 lint、Vitest、TypeScript 检查和生产构建。依赖版本以 `package-lock.json` 为准，不使用未写入锁文件的全局包。

## 开发模式

需要热更新时才使用两个终端：

```powershell
# 终端 1
cargo run --locked
```

```powershell
# 终端 2
cd frontend
npm run dev
```

日常生产运行不需要 Vite，只需在仓库根目录执行 `cargo run --release --locked`。

## 目录

```text
src/api/          HTTP API 封装
src/components/   通用组件
src/composables/  可复用状态与交互逻辑
src/router/       页面路由
src/stores/       Pinia 状态
src/views/        登录、浏览器和后台页面
src/styles/       主题与布局样式
src/test/         测试环境和共享工具
```

图标统一使用 `@phosphor-icons/vue`。不要引入远程字体、远程脚本或第二套图标系统。权限判断必须由后端执行，前端的按钮显示只用于改善交互。
