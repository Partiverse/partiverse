# ADR-0003 core 拷贝件声明(partisync caps.rs/error.rs,Apache-2.0)

- 状态: 接受 | 日期: 2026-10-09 | 负责人: @partiverse(草稿见 docs/reports/runs/M1-WP01-T01.md §3,escalate 裁定移交本卡落档)
- 背景: SPEC §6 复用边界允许 V1 以「拷贝+标注」方式复用 partisync core 的 caps.rs 与 error.rs 作骨架;M1-WP01-T01 已执行拷贝入库。作业红线要求禁止引用许可证不明的代码,SPEC §6 要求文件头标注来源与许可,故须以 ADR 固化拷贝件溯源声明。
- 决策: `crates/partiverse-core/src/caps.rs`(103 行)与 `crates/partiverse-core/src/error.rs`(98 行)拷贝自 https://github.com/Partiverse/partisync 的 `crates/partisync-core/src/`,基线 commit `d7f73ffd516a`(Apache-2.0,Copyright PartiSync contributors);两文件各追加 10 行标注头(版权、来源仓库与原路径、基线 commit、许可、改动声明),标注头之后与上游源文件逐字节一致(T01 以 cmp 校验);改动仅限模块归属适配,实际零代码改动。NOTICE 第 14 行已由仓库 init 预置对应条目。
- 理由: 依据 docs/reviews/M1-partisync复用评估.md「方案 b」(V1 代码复用例外及 §2 裁定表):error.rs 的 Severity 分级错误骨架与 caps.rs 的能力协商模型经 partisync 生产验证,重写无增量收益且引入行为分叉风险;上游为同组织 Apache-2.0 项目,保留版权与许可标注、NOTICE 列明后,拷贝入本仓库 AGPL-3.0 代码库许可兼容。被否选项:链接依赖 partisync 仓库(跨仓库版本耦合,否决);仅作参考重写(放弃已验证骨架,与评估裁定不符,否决)。
- 影响: 正向=WP03(错误映射骨架)与 WP09(caps 能力协商)复用已验证实现,降低先行风险;代价=继承上游品牌类型名(如 `PartisyError`)与陈旧 SPEC 引用,T01 §4 已登记,建议 WP03 排期清理;波及面=后续从上游取新拷贝时,必须更新基线 commit 标注并重新逐字节比对。

## 修订 1(2026-10-09,M1-WP03-T01)

- 触发: 本 ADR 影响条「建议 WP03 排期清理」的到期落地。任务卡 M1-WP03-T01 裁定走最保守路径:仅追加品牌别名,不改拷贝件本体(改名 `PartisyError`→`PartiverseError` 属拷贝件改动,需重新逐字节比对面与上游同步策略调整,收益不足,否决)。
- 决策: `crates/partiverse-core/src/error.rs` 末尾追加 3 行注释 + 1 行别名 `pub type PartiverseError = PartisyError;`(追加段);标注头「本仓改动」行同步补记追加段。
- 比对口径修订: 自本修订起,「逐字节一致」的比对范围 = 标注头之后的**上游原文段**(至 EOF 追加段分隔线注释前),追加段不计入比对;`caps.rs` 不受影响(仍为标注头外整文件逐字节一致)。
- 理由: 别名零行为变更、零 API 破坏(WP03 客户端错误映射开始即可用本仓品牌名引用),同时保持拷贝件与上游 `d7f73ffd516a` 的可追溯性;后续如上游演进,取新拷贝时仍按原基线流程更新并重划追加段。
