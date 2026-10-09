# ADR-0003 core 拷贝件声明(partisync caps.rs/error.rs,Apache-2.0)

- 状态: 接受 | 日期: 2026-10-09 | 负责人: @partiverse(草稿见 docs/reports/runs/M1-WP01-T01.md §3,escalate 裁定移交本卡落档)
- 背景: SPEC §6 复用边界允许 V1 以「拷贝+标注」方式复用 partisync core 的 caps.rs 与 error.rs 作骨架;M1-WP01-T01 已执行拷贝入库。作业红线要求禁止引用许可证不明的代码,SPEC §6 要求文件头标注来源与许可,故须以 ADR 固化拷贝件溯源声明。
- 决策: `crates/partiverse-core/src/caps.rs`(103 行)与 `crates/partiverse-core/src/error.rs`(98 行)拷贝自 https://github.com/Partiverse/partisync 的 `crates/partisync-core/src/`,基线 commit `d7f73ffd516a`(Apache-2.0,Copyright PartiSync contributors);两文件各追加 10 行标注头(版权、来源仓库与原路径、基线 commit、许可、改动声明),标注头之后与上游源文件逐字节一致(T01 以 cmp 校验);改动仅限模块归属适配,实际零代码改动。NOTICE 第 14 行已由仓库 init 预置对应条目。
- 理由: 依据 docs/reviews/M1-partisync复用评估.md「方案 b」(V1 代码复用例外及 §2 裁定表):error.rs 的 Severity 分级错误骨架与 caps.rs 的能力协商模型经 partisync 生产验证,重写无增量收益且引入行为分叉风险;上游为同组织 Apache-2.0 项目,保留版权与许可标注、NOTICE 列明后,拷贝入本仓库 AGPL-3.0 代码库许可兼容。被否选项:链接依赖 partisync 仓库(跨仓库版本耦合,否决);仅作参考重写(放弃已验证骨架,与评估裁定不符,否决)。
- 影响: 正向=WP03(错误映射骨架)与 WP09(caps 能力协商)复用已验证实现,降低先行风险;代价=继承上游品牌类型名(如 `PartisyError`)与陈旧 SPEC 引用,T01 §4 已登记,建议 WP03 排期清理;波及面=后续从上游取新拷贝时,必须更新基线 commit 标注并重新逐字节比对。
