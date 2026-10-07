# Third-party notices

English | [简体中文](THIRD_PARTY_NOTICES.zh-CN.md)

Ycloud uses the open-source components below. Exact versions are pinned in `Cargo.lock` and `frontend/package-lock.json`; this is a list of the main projects, not every dependency.

| Project | License |
| --- | --- |
| Vue, VueUse, Reka UI, Phosphor Icons, Tailwind CSS, tailwind-merge, clsx, shadcn-vue components | MIT |
| class-variance-authority | Apache-2.0 |
| Axum, Tokio, Tower, tracing, bytes, http-body, http-body-util, mime_guess, async_zip | MIT |
| Serde, serde_json, Chrono, UUID, anyhow, Argon2, base64, rand_core, futures-util, fs4, qrcode, windows-sys | MIT or Apache-2.0 |
| rustix | MIT or Apache-2.0, with an additional LLVM exception option |
| roxmltree | MIT or Apache-2.0 |
| AWS SDK for Rust, AWS Smithy | Apache-2.0 |
| ring | Apache-2.0 and ISC |

Required copyright notices, license texts, and NOTICE files for distributed code are collected in [Third-party license texts](THIRD_PARTY_LICENSES.md). Identical license terms are included once, with each component's copyright notices retained. The appendix covers the built frontend and the current normal backend dependencies for x86_64 Windows and Linux. Test dependencies and build-only tools are not included.

Distribute `THIRD_PARTY_NOTICES.md` and `THIRD_PARTY_LICENSES.md` with executables, frontend bundles, and container images. Review the appendix when dependencies, target platforms, or features change. System libraries and container OS packages have their own distribution copyright files. These third-party licenses are not a license for Ycloud itself.
