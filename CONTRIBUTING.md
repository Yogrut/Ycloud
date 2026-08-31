# Ycloud 开发规则

## 环境

- Rust 1.96.1，最低兼容版本 1.94.1。
- Node.js 版本以 `frontend/package.json` 的 `engines` 为准。
- 使用 `Cargo.lock` 和 `frontend/package-lock.json`。

## 提交前检查

仓库根目录：

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --locked
cargo build --release --locked
cargo audit
cargo deny check
```

`frontend/` 目录：

```bash
npm ci
npm run check
npm audit --audit-level=moderate
```

前端构建必须同步更新 `static/app`。

## 代码规则

- 权限、路径、容量和上传票据由后端验证。
- 异步文件操作在执行前冻结 `storage_id` 和规范化路径。
- 存储操作通过 `StorageRegistry` 和存储后端接口执行。
- 配置字段变更必须增加 schema 迁移和旧配置测试。
- 凭据只通过统一秘密持久化层读写。
- API 错误格式保持 `{ "error": { "code", "message" } }`。
- 新增稳定错误码时同步增加后端和前端测试。
- 模块按业务职责拆分，不增加只转发调用的抽象层。
- 不提交配置、凭据、用户数据、日志、`target/` 或 `node_modules/`。

## S3 集成测试

真实 S3 测试只使用隔离 Bucket/Prefix 和专用凭据。默认测试不得访问外部存储。
