# AGENTS.md

本项目的 AI 协作说明位于 [AI Config](AI%20Config/README.md)。修改代码前阅读该目录的 `README.md` 和 `RULES.md`；交付前按 `CI_CHECKS.md` 运行与改动范围相符的检查。

## 工作方式

- 先检查现有实现、调用点和测试，再改动。尤其是路径比较、权限校验、请求错误处理和异步响应，避免新增第二套语义。
- 以当前代码和实际 CI 为事实来源。文档与实现冲突时，先核实，再更新相关文档；不要把旧行号、旧测试数当作现状。
- 保留工作区已有改动。只修改本次任务相关文件，不顺手重构无关模块。
- 不把代码搜索命中数自动判定为缺陷。区分真实逻辑、HTTP handler、测试代码和已有例外。

## 验证

Rust 代码改动运行：

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --locked
```

前端代码改动在 `frontend/` 运行 `npm run check`，确认构建后的 `static/app/` 产物也已更新。文档改动检查链接、路径、命令和 diff 即可。报告实际执行结果；未运行的检查不要声称通过。
