# AI Config

这里记录 Ycloud 的 AI 协作规则与验证办法，不保存某次审计的命中数、测试通过数、行号清单或“当前缺陷基线”。这些数据会随代码变化，必须在本次任务中重新检查。

## 阅读顺序

1. [RULES.md](RULES.md)：语言与框架的官方建议，以及明确标记的项目约定。
2. [CI_CHECKS.md](CI_CHECKS.md)：当前 CI 命令、适用范围与人工复核方式。
3. [ARCHITECTURE.md](../ARCHITECTURE.md) 和 [CONTRIBUTING.md](../CONTRIBUTING.md)：涉及架构或贡献流程时再读；与代码不一致时应核实并同步修订。

## 当前项目边界

- 后端是 Rust 服务；包信息和声明的最低 Rust 版本以 [Cargo.toml](../Cargo.toml) 为准，开发工具链以 [rust-toolchain.toml](../rust-toolchain.toml) 为准。
- 前端是 Vue、TypeScript 和 Vite 项目。页面代码在 `frontend/src/apps/`，共享 API、路由、样式和组件在 `frontend/src/shared/`。具体脚本以 [frontend/package.json](../frontend/package.json) 为准。
- `static/app/` 是提交到仓库的前端构建产物；修改前端后应运行构建并检查产物。实际 CI 步骤以 [工作流](../.github/workflows/frontend.yml) 为准。
- 本目录不代替用户需求、代码审查或测试，也不要求顺手修复与本次任务无关的历史问题。

## 如何处理旧问题

搜索命中只提示“需要检查”，不等于缺陷。例如 `get_` 可能是 HTTP GET handler；读取源码的测试可能是在校验项目资源；不同路径比较函数可能服务于不同语义。先查看上下文、实际调用点和对应测试，再决定是否修改。

如果发现文档与当前实现冲突，以可验证的代码和运行结果为依据，修订相应描述。不要把未验证的风险推断写成已证实的漏洞。

## 来源与层级

Rust 命名和 API 设计参考 [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) 与 [Rust Style Guide](https://doc.rust-lang.org/style-guide/)；Vue 组件约定参考 [Vue Style Guide](https://vuejs.org/style-guide/)；类型约束参考 [TypeScript Handbook](https://www.typescriptlang.org/docs/handbook/) 和项目配置。官方建议、项目已有约定、工具能验证的事实应分开描述；用户本次明确要求优先于通用风格偏好。
