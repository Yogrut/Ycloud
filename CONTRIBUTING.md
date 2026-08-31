# Ycloud 开发与变更准则

## 开发环境

- Rust 会由 `rust-toolchain.toml` 固定为 1.96.1；项目声明的最低兼容版本是 1.94.1。
- Node.js 使用 `frontend/package.json` 的 `engines` 范围，依赖必须通过 `npm ci` 从锁文件安装。
- 运行配置、凭据、用户文件、审计日志、`target/` 和 `node_modules/` 都不能提交。

## 提交前检查

在仓库根目录执行：

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --locked
cargo build --release --locked
cargo audit
cargo deny check
```

在 `frontend/` 执行：

```text
npm ci
npm run check
npm audit --audit-level=moderate
```

前端生产构建写入 `static/app`，这是 Rust 可执行文件嵌入的正式页面。构建完成后，`git diff -- static/app` 必须只包含与本次前端源码对应的产物变化。

## 边界规则

- 权限、路径、容量和上传批次必须由后端验证；前端状态只负责交互，不能成为安全边界。
- 任何异步文件操作都必须冻结 `storage_id` 和规范化路径，不能在 await 之后重新读取用户当前选择的存储。
- S3、本地存储和 WebDAV 必须通过既有存储与授权抽象；不要在 handler 内绕过注册表直接拼接路径或对象键。
- 配置字段变化必须增加 schema 迁移和旧配置回归测试；凭据字段只能经过统一秘密持久化层。
- API 错误维持 `{ "error": { "code", "message" } }`；新增稳定错误类型时同时补后端和前端测试。
- 模块拆分以完整业务职责为单位。不要为了文件数量添加只转发调用的 service/repository 层。

## S3 集成测试

默认测试不会访问真实对象存储。需要在隔离测试桶运行被忽略的 S3 冒烟测试时，必须使用专用凭据和可清空前缀，并显式执行对应 ignored test；不要把生产桶或生产凭据放入测试环境。
