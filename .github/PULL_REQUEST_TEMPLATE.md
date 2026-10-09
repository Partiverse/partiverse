<!--
标题请遵循 AGENTS.md 提交与追溯格式:
<type>(<scope>): <subject> [P{p}-WP{nn}-T{nn}]
正文引用规格:Spec: docs/specs/P{p}-WP{nn}.md
-->

## 变更说明

<!-- 做了什么、为什么;引用任务卡路径(docs/tasks/…)与对应 SPEC 章节 -->

## 自查清单

- [ ] 提交挂 Task-ID(`Task-ID: P{p}-WP{nn}-T{nn}`),未跨任务卡夹带(铁律 2/9)
- [ ] diff ≤400 行(铁律 3;L 卡 ≤1000 行豁免以任务卡为准)
- [ ] 本地五门禁绿:`cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` / `pnpm build` / `cargo deny check`
- [ ] 测试证据随附(新行为测试先行;未删除/跳过测试、未放宽断言)
- [ ] GUI 变更已实际操控验证并附截图归档 `docs/screenshots/`(如适用;纯后端可标注「不适用」)
- [ ] 未触碰门禁阈值、deny.toml、rust-toolchain.toml(改动需 ADR)
- [ ] 无 TODO/占位/桩实现冒充交付;无生产路径 unwrap/expect;无吞错/静默降级
- [ ] GUI 文案仅英文(资源化前);代码注释/提交信息中文;无凭据/密钥入库

## 验证证据

<!-- 粘贴实跑命令与关键输出(测试结果/截图链接);无法实跑的项显式说明 -->
