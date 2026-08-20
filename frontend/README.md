# Ycloud Vue 前端

这是 Ycloud 的 Vue 3.5 候选前端。当前阶段仅迁移首页登录与共享主题，旧版内嵌前端仍是生产入口和回退方案。

## 本地运行

先在仓库根目录启动 Rust 服务（默认 `127.0.0.1:18473`），再执行：

```powershell
cd frontend
npm ci
npm run dev
```

访问 `http://127.0.0.1:5173/v2/`。开发代理只转发 Ycloud 的同源 API，不使用外部 CDN。

## 依赖策略

- `package.json` 使用精确版本，`package-lock.json` 必须提交，安装使用 `npm ci`。
- 每周 Dependabot 只提出分组更新；任何更新都必须通过 lint、测试、类型检查和生产构建后人工合并。
- Vue 运行时与构建、检查、测试工具分组升级，降低一次更新的影响范围。
- TypeScript 暂固定为 `6.0.3`。虽然 7.x 已发布，但 `typescript-eslint 8.67.0` 的正式兼容范围仍是 `<6.1.0`，在其上游完成支持前不越过该边界。
- 不使用 `latest`、不自动合并、不为追求版本号绕过 peer dependency 检查。

## 检查

```powershell
npm run check
npm audit
```
