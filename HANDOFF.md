# Ycloud 交接

## 当前状态

- 路径：`D:\SYStemFiles\文档\open_code\Ycloud-v2`
- 分支：`main`
- 清理前检查点：`3480827b870aed6a811729ee14c8de2e2e34a82e`
- 已实现：Vue 3.5 前端、多存储、本地事务存储、S3、普通账号按存储授权、WebDAV、文件夹锁、传输限制和登录安全。
- 清理前检查：Rust 83 项通过；前端 22 个文件、88 项测试通过；lint、类型检查和构建通过。

## 本次清理

- 删除 `.agents/`、`.obsidian/`、`skills-lock.json`、旧计划和旧术语文档。
- 删除可重建的 `target/` 与 `frontend/node_modules/`。
- 运行配置、日志和用户存储已移到：
  `D:\SYStemFiles\文档\open_code\Ycloud-v2-runtime-backup-20260827-011511`
- 项目内 `storage/` 只保留 `.gitkeep`。

## 约束

1. 安全 40%、性能 30%、稳定 30%，数据安全与完整性优先。
2. 普通账号只能由管理员创建、改密、启停和授权，不能进入后台。
3. 权限按 `storage_id` 隔离，不能合并不同存储命名空间。
4. 不绕过存储接口访问磁盘或对象，不把完整大文件读入内存。
5. 不提交密钥、运行配置、日志、真实用户文件和测试报告。
6. 不修改 `D:\SYStemFiles\文档\SRE`。

## 下一步

1. 完成本次清理后的全套检查并提交。
2. 用临时配置测试本地存储、普通账号权限和 WebDAV。
3. 补多存储切换、S3 失败恢复和权限边界的端到端测试。
4. 真实存储先只读验证，再开放写入。

启动和检查命令见 [README.md](README.md)，结构以 [ARCHITECTURE.md](ARCHITECTURE.md) 和当前代码为准。
