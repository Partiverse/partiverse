# ADR-0001 Rust 工具链钉版(cargo/rustc 1.99.0)

- 状态: 接受 | 日期: 2026-10-09 | 负责人: @partiverse(工具链文件随 M1-WP01-T01 落地并经 Owner 终审;本 ADR 为其决策记录)
- 背景: 仓库是多环境作业模式(本地容器 + GitHub Actions 三平台 runner),rustc/cargo 版本漂移会改变 rustfmt 格式化结果与 clippy lint 集,导致「五门禁」结果在不同环境不可比、main 可复现性失效。AGENTS.md 作业红线已规定 rust-toolchain.toml 变更需 ADR;M1-WP01-T01 已将 rust-toolchain.toml(channel=1.99.0、profile=minimal、components=[rustfmt, clippy])落入栈内,本 ADR 补记该决策。
- 决策: 以仓库根 `rust-toolchain.toml` 为唯一工具链事实源:channel=1.99.0、profile=minimal、components=[rustfmt, clippy];本地与 CI 一律经 rustup 代理按该文件自动选用(CI 用 `dtolnay/rust-toolchain@1.99.0` 显式安装同名版本并补齐 rustfmt/clippy 组件,见 .github/workflows/ci.yml)。
- 理由: ①可复现性——fmt/clippy/test/deny 四门禁的判定结果依赖工具链版本,钉精确版是「门禁绿」语义稳定的前提;②minimal profile 只装编译所需组件,缩短 CI 安装时长;③被否选项——a) 跟随 runner 默认 stable:lint 集每日可能漂移,无预警打断 main,否决; b) 钉 stable 通道名:同样随上游发布漂移,否决; c) 只声明 MSRV 下限不钉版:M1 仅 4 个零第三方依赖成员,精确钉版比 MSRV 兼容性矩阵更简单直接。
- 影响: 正向=三平台构建可复现、门禁结果跨环境可比;代价=升级工具链须改 rust-toolchain.toml 并更新本 ADR(红线条目,双人评审);波及面=全部 Rust 成员(crates/partiverse-*、xtask)、CI 工具链安装步骤、贡献者本地 rustup(进入仓库自动生效)。
- 许可证: Rust 工具链 Apache-2.0/MIT 双许可,无隐含商业授权;上游 rust-lang/rust 高频活跃,无停更风险。
