# RULES — Ycloud 代码约定

本文件保留长期有效的规则，不记录某一时刻的违规数量或待修复清单。规则来源分为“语言/框架建议”和“项目现有做法”；两者不能混为一谈。用户本次明确要求与一般风格建议冲突时，先满足用户要求并说明取舍。

## Rust

参考 [Rust Style Guide](https://doc.rust-lang.org/style-guide/) 和 [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)。

- 格式以 `cargo fmt --all -- --check` 为准。项目当前没有自定义 rustfmt 配置；不要仅为个人偏好引入新格式规则。
- 新命名遵循 Rust 惯例：类型用 `UpperCamelCase`，函数、方法和模块用 `snake_case`。转换方法可参考 API Guidelines 的 `as_`、`to_`、`into_` 语义；不要仅凭名称批量重命名现有函数。
- `get_` 搜索结果需按用途判断。普通 getter、HTTP GET handler、`get_or_insert` 等不能混判。
- 新增公开 API 时提供有用的文档，重点写清不直观的错误条件、panic 或安全约束。现有 `pub fn` 的简单文本统计不能衡量文档质量，也不是全仓重构任务。
- 为公开类型考虑 `Debug`，但敏感配置应避免在调试输出里泄露凭据；可使用手写的脱敏实现。不能用 `derive(Debug)` 次数与类型次数相减来认定缺陷。
- 参数校验、权限判断、存储路径处理和错误转换要复用现有模块中的语义；改动前搜索调用点及测试。不同用途的路径函数不应仅因名称相近就合并。

## Vue 与 TypeScript

参考 [Vue Style Guide](https://vuejs.org/style-guide/) 和 [TypeScript Handbook](https://www.typescriptlang.org/docs/handbook/)；可执行约束以项目 ESLint、`vue-tsc` 与测试配置为准。

- 新组件优先使用多词名称；根组件 `App` 是 Vue 官方例外。仓库已有的单词 UI 组件属于既有代码，修改它们时评估兼容性，不把批量改名隐含进无关任务。
- `v-for` 提供稳定 `key`，不要在同一元素同时使用 `v-if` 与 `v-for`。Props 写出类型。
- 修改 UI 时遵循现有主题变量、布局和组件交互。项目并非所有控件都使用圆角；按控件角色和相邻页面保持一致，不机械套用一种造型。
- 页面请求优先复用 `frontend/src/shared/api/` 的现有客户端。401、超时、错误展示、异步请求竞态等处理要先检查现有策略，不在新页面另起一套。
- 路由路径读取和规范化先看 `frontend/src/shared/routes.ts`。涉及文件路径、权限或范围比较时，先明确各函数的输入约束和平台语义。
- 修改中英文文案时检查 `frontend/src/shared/i18n/` 与现有页面做法，避免引入新的互不兼容的翻译机制。
- 不把“测试读取源码文本”一概视作错误；行为测试、资源一致性测试分别按其目的评估。新增测试应尽量验证实际行为或稳定契约。

## 项目约定与边界

- 后端代码在 `src/`；前端应用和共享代码在 `frontend/src/`；提交的浏览器产物在 `static/app/`。实际路径以仓库现状为准，不依赖旧行号。
- `Cargo.toml` 声明最低 Rust 版本，`rust-toolchain.toml` 与 CI 指定当前验证工具链。两者含义不同；不要将 CI 通过等同于最低版本已验证。
- 变更尽量局部，保持 API、测试与文档一致。发现历史问题可以报告；未获本次任务授权时不顺手扩大修改范围。
- 对安全或兼容性问题，区分事实、测试结果与推断。不能只凭一次 `rg` 命中就宣布漏洞或“已修复”。

验证命令与人工复核清单见 [CI_CHECKS.md](CI_CHECKS.md)。
