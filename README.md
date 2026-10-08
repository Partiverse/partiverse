# Partiverse(暂定名)

聚合主流云端存储与本地存储的统一文件浏览器。

## 文档

| 文档 | 内容 |
|---|---|
| [00-术语表](./docs/00-术语表.md) | 域模型术语权威定义:Node(存储端)/Particle(资产原子单元)/Placement(驻留)及与 partisync 的映射 |
| [01-市场调研报告](./docs/01-市场调研报告.md) | 市场规模、用户痛点、国内外竞品、监管环境、机会判断 |
| [02-可行性分析报告](./docs/02-可行性分析报告.md) | 技术/商业/法律三维可行性、风险矩阵、合规红线清单 |
| [03-开发计划](./docs/03-开发计划.md) | 产品定位、版本规划、技术架构、里程碑、成本、决策清单(D1-D24) |
| [04-rclone选型评估](./docs/04-rclone选型评估.md) | rclone 作多云引擎的深度尽调:集成方式、后端覆盖、挂载硬伤、法律面、对纯 AI 开发的适配性 |
| [05-PartiSync协作评估](./docs/05-PartiSync协作评估.md) | 姊妹项目 [Partiverse/partisync](https://github.com/Partiverse/partisync)(本地资产智能底座)的深度调研与两产品协同方案:引擎接缝、crate 级复用、流程继承、能力边界宪法(§9.5) |
| [06-架构设计-Phase0草案](./docs/06-架构设计-Phase0草案.md) | 总体拓扑、引擎生命周期、凭据安全、索引/传输编排、Node 能力画像子系统(§12) |
| [07-商业授权审计与备选方案](./docs/07-商业授权审计与备选方案.md) | 全部隐含商业授权点的首选/备选/自研保底三级方案与许可证清洁栈一览 |
| [08-前端UIUX规划](./docs/08-前端UIUX规划.md) | 设计原则、分阶段 UIUX、UI 基座选型(shadcn+TanStack)、V1-V4 长远铺垫 |
| [09-场景模式设计](./docs/09-场景模式设计.md) | 场景框架与"场景即配置"机制:照片馆/影视库/音乐厅/书房/文档库的呈现与分期、第三方开放路径 |
| [10-产品蓝图](./docs/10-产品蓝图.md) | M1→M4+ 交付形态演进(V4=桌面深度期)、闸门制、跨期宪法、交付 Benchmark |
| [11-开发前敲定清单](./docs/11-开发前敲定清单.md) | D28-D36 开工前决策项(品牌占位/语言政策/引擎分发中国镜像/打包/密钥/文档分仓/开源商业边界)+工程注意事项 |

## 状态

**M1(WP01 骨架与门禁)开发中。** 决策 D1-D37 全部关账(2026-10-08);M1-WP00 SPEC v0.3 与 D28-D36 已获 Owner 批准。

- 决策要点:国内优先;纯官方 API+标准协议(123 仅 WebDAV);桌面三端 Tauri+React(shadcn 基座);开源核心(AGPL-3.0)+Pro;rclone 引擎;与 [PartiSync](https://github.com/Partiverse/partisync) 互补协作+能力边界宪法(D27);场景框架(D26);开发=partisync 规格驱动+ZCode Workflow(执行方案 §8)。
- 保密纪律:产品中文名保密中(仓库内已脱敏);GUI 语言=English only(D29);品牌资产=占位(D28,REPLACE-BEFORE-RELEASE 为发布 gate)。
- 当前:M1-WP01 任务卡已立(T01 工作区/T02 应用壳/T03 CI),按 Workflow 模板执行;Phase 0 关账条件=Owner 完成百度/123 提交+法务终审;简报节奏见 [P0-report](./docs/reports/P0-report.md)。
