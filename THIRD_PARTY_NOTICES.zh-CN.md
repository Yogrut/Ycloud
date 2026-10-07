# 第三方开源组件

[English](THIRD_PARTY_NOTICES.md) | 简体中文

Ycloud 使用以下开源组件。准确版本由 `Cargo.lock` 和 `frontend/package-lock.json` 固定；本文件只列主要项目，不逐个介绍。

| 项目 | 许可证 |
| --- | --- |
| Vue、VueUse、Reka UI、Phosphor Icons、Tailwind CSS、tailwind-merge、clsx、shadcn-vue 组件 | MIT |
| class-variance-authority | Apache-2.0 |
| Axum、Tokio、Tower、tracing、bytes、http-body、http-body-util、mime_guess、async_zip | MIT |
| Serde、serde_json、Chrono、UUID、anyhow、Argon2、base64、rand_core、futures-util、fs4、qrcode、windows-sys | MIT 或 Apache-2.0 |
| rustix | MIT 或 Apache-2.0（另提供 LLVM exception 选项） |
| roxmltree | MIT 或 Apache-2.0 |
| AWS SDK for Rust、AWS Smithy | Apache-2.0 |
| ring | Apache-2.0 与 ISC |

随产品分发的代码所需版权、许可文本和 NOTICE 统一保存在 [许可附录](THIRD_PARTY_LICENSES.md)，相同许可条款合并保存，并保留各组件的版权信息。附录包含实际前端产物，以及当前 x86_64 Windows/Linux 后端普通依赖的声明；测试依赖、纯构建工具不列入。

分发可执行文件、前端产物或容器时，应一并携带英文默认版 `THIRD_PARTY_NOTICES.md` 与 `THIRD_PARTY_LICENSES.md`。依赖、目标平台或功能发生变化后，应重新核对附录；系统库和容器操作系统组件由对应发行包的版权文件说明。这些许可证不等于 Ycloud 自身的项目许可证。
