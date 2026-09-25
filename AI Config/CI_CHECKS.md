# CI_CHECKS — 验证与复核

这里记录当前仓库可重复执行的检查，不保存某次运行的通过数或搜索命中数。命令与平台分工以 [CI 工作流](../.github/workflows/frontend.yml) 为准；脚本以 [frontend/package.json](../frontend/package.json) 为准。

## Rust 改动

在仓库根目录运行：

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --locked
```

CI 在 Linux 运行格式与 Clippy，在 Linux 和 Windows 运行 Rust 测试与 release 构建。仓库的 `rust-toolchain.toml` 指定 1.96.1；`Cargo.toml` 另声明 `rust-version`，CI 使用固定工具链并不等于已经验证最低版本。若本机因旧 `target/` 权限而失败，可临时设置独立的 `CARGO_TARGET_DIR` 后重试，并报告原因。

## 前端改动

在 `frontend/` 运行：

```text
npm run check
```

该脚本依次运行 lint、Vitest、类型检查及 Vite 构建。构建会更新仓库提交的 `static/app/`；检查这些产物是否与源码对应，并审阅产物 diff。CI 还运行 `npm audit --audit-level=moderate`，这是依赖审计，不替代功能测试。

## 容器与配置改动

若改动 Docker、Compose 或部署配置，按需运行：

```text
docker compose config --quiet
docker build --pull --tag ycloud:check .
```

CI 还检查生产镜像的非 root 用户并启动容器。不要把未在本地完成的容器检查写成“通过”。

## 文档改动

只修改 Markdown 或 AI 协作说明时，检查相对链接、引用路径、命令与当前仓库是否一致，并运行 `git diff --check`。未跟踪文件不在普通 `git diff` 中，应单独检查内容和行尾空白。无需仅因文档文字改动重跑整套编译测试。

## 人工复核：只对本次改动相关的项执行

以下搜索是定位线索，**命中不等于违规**；没有固定的允许次数，也没有“新增即失败”的历史基线。先看调用上下文和测试。

| 涉及内容 | 可用搜索或检查 | 复核重点 |
| --- | --- | --- |
| Rust API 命名与文档 | `rg -n 'fn get_|fn (as_|to_|into_)|^\s*pub (async )?fn ' src` | 区分 getter、HTTP handler、转换方法；检查新增公开 API 的说明 |
| 路径与权限 | `rg -n 'path_is_same_or_descendant|paths_overlap|normalize_relative' src` | 输入约束、大小写与包含语义是否与调用方一致 |
| 前端路由与认证 | `rg -n 'currentAppPath|pathname|status === 401' frontend/src` | 是否复用已有路由与错误处理，是否遗漏竞态防护 |
| Vue 模板 | 检查改动组件的 `v-for`、`key`、`v-if`、Props 与名称 | Vue Style Guide 的相关规则；根 `App` 例外 |
| 测试 | 查看新测试实际断言对象 | 行为测试与资源一致性测试目的不同，不能靠 `readFileSync` 命中判错 |
| 前端构建产物 | `git status --short -- static/app`、构建产物 diff | 源码与提交产物同步 |
| 文档 | `rg -n 'ARCHITECTURE|CONTRIBUTING|rust-version|npm run check' 'AI Config' AGENTS.md` | 文档引用和版本表述与当前仓库匹配 |

交付时简要写明实际运行的命令及结果、未运行项和原因；若发现文档/代码冲突，说明已如何处理。不要沿用旧的测试数、搜索命中数或行号。
