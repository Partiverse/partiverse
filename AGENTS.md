# AGENTS.md — AI 代理作业规范(根上下文)

> 本文件是所有 AI 编码代理在本仓库的第一上下文。开发方式沿用姊妹项目
> [PartiSync](https://github.com/Partiverse/partisync) 已验证的规格驱动模式
> (其治理总纲 `docs/partisync-AI开发执行方案.md` 为母本;本仓库专属差异在本文档内声明)。
> 项目规划文档:[01 市场调研](./docs/01-市场调研报告.md) · [02 可行性](./docs/02-可行性分析报告.md)
> · [03 开发计划](./docs/03-开发计划.md) · [04 rclone 选型](./docs/04-rclone选型评估.md)
> · [05 PartiSync 协作评估](./docs/05-PartiSync协作评估.md) · [06 架构设计草案](./docs/06-架构设计-Phase0草案.md)

## 项目速览

- **Partiverse**:聚合主流云端存储与本地存储的统一文件浏览器(桌面优先,Tauri 2 + React + Rust,多云引擎 = rclone sidecar)。
- 当前阶段:**Phase 0 立项与设计**(未写产品代码;见 [03 开发计划 §5](./docs/03-开发计划.md))。
- 产品命名:英文 Partiverse;**中文名已内部暂定、保密中**——商标/撞名核实与 Owner 定名完成前,
  不得在任何公开物料(README、官网、提交信息、issue、宣传文案)中出现;仓库文档一律以「产品中文名(保密代号)」指代。

## 十条铁律(继承 partisync,全文将以《开发执行方案》固化)

1. 规格先行:无已批准 SPEC 不写实现。
2. 追溯唯一:提交必须挂 Task-ID。
3. 原子交付:单 PR ≤400 行 diff。
4. 测试即规格:行为契约先于实现。
5. 门禁前置:CI 红灯禁止合入,覆盖率只升不降(M0 建立 CI 后生效)。
6. 双通道审查:AI 对抗审查 + 人工终审(Owner)。
7. 地基人工:schema/密码学/unsafe/公共 API 双人评审(Owner + 独立 AI 复核)。
8. 无聊依赖:新依赖需 ADR + cargo deny(workspace forbid unsafe;禁用停更依赖);**任何隐含商业授权的依赖(付费许可/年费/审核费/需主体证书)必须同步给出备选方案(开源替代,自研为最终保底)并更新 [商业授权审计表](./docs/07-商业授权审计与备选方案.md),无备选不得合入**。
9. 会话即弃:一任务一会话,不越任务卡声明的文件清单。
10. 审计即工件:里程碑不出规定工件 = 未完成。

## 提交与追溯格式(强制,与 partisync 一致)

```
<type>(<scope>): <subject> [P{p}-WP{nn}-T{nn}]

Task-ID: P{p}-WP{nn}-T{nn}
Spec: docs/specs/P{p}-WP{nn}.md
AI-Assist: <agent/model> (做了什么)
AI-Review: <agent/model>            # 有 AI 对抗审查时
Reviewed-By: <owner-id>             # 人工终审后
```

编号:Phase 0 用 `P0-*`;产品里程碑阶段沿用 `M{m}-WP{nn}-T{nn}`。分支 `p0/wp00-t01-slug`。

## 产品合规红线(硬约束,违反即评审否决;完整清单见 02 报告 §4.4)

1. 只使用官方开放平台 API 与标准协议;**不内置任何逆向协议驱动**;社区驱动走用户自装插件并显式风险提示。
2. 不破解、不限速规避、不批量爬取;请求频率尊重各家配额(配额预算编排层强制)。
3. 不做资源推荐/热搜/分享广场;不缓存、不存储用户文件内容;营销话术避开「搬运/迁移/加速」。
4. 凭据与索引只存用户设备,凭据必须入 OS 凭据库或 rclone 加密 config,**禁止明文落盘、禁止经我方服务器**。
5. **中文名保密条**:见「项目速览」;开源核心(AGPL-3.0)公开发布前 Owner 确认命名。

## 作业红线(禁止事项)

- 禁止修改 CI 门禁阈值、deny.toml 白名单、rust-toolchain.toml(均需 ADR)。
- 禁止删除/跳过测试、放宽断言来「让测试变绿」——放宽断言需独立 PR 与理由。
- 禁止凭记忆编写第三方 crate/API——必须查证该版本文档/源码后使用(rclone 配置项以所用版本文档为准)。
- 禁止引用或逐字复制许可证不明的代码;OpenList 系(AGPL)代码仅可作兼容性参考,禁止链接复用。
- 禁止 unsafe(workspace forbid;需要时先提 ADR)。
- 禁止改动任务卡声明文件清单之外的文件(「顺手修」一律另开任务)。
- **禁止走捷径(✅ Owner 指令 2026-10-08)**:TODO/占位/桩实现冒充交付(mock 仅限测试且命名 Fake*)、吞错或静默降级、生产路径 unwrap/expect、硬编码端点/路径/密钥(如 123 WebDAV 域名须用户粘贴)、卡内未声明的实现决策自行发挥——一律视为违规而非裁量,评审否决。
- **GUI 语言一致性(✅ Owner 指令 2026-10-08)**:GUI 文案一律走资源文件,唯一语言包=English(i18n 资源化落地 zh-CN 前,**只使用英文,严禁半英半中**);GUI 验收截图出现混排=不通过。代码注释/commit=中文(组织惯例),标识符/日志 key/错误码(P-ERR-xxx)=英文。
- **品牌占位纪律(✅ Owner 指令 2026-10-08)**:仓库内一切 logo/图标/品牌色为**占位资产**(assets/brand/,标注 PLACEHOLDER,确定性生成);REPLACE-BEFORE-RELEASE.md 为发布 gate——正式资产未替换前不得公开发布;显示名公开渠道仅 "Partiverse"(中文名保密纪律见上)。
- 涉及 OpenAI/Google 等外部服务的凭据、测试账号信息禁止入库(含 docs)。

## 计划仓库结构与依赖方向(自上而下只允许向下依赖)

```
app/            # Tauri 壳 + React 前端(D6:React + TypeScript)
core/           # Rust:统一索引(SQLite+FTS5)/ 配额预算编排 / 传输编排(rc job)/ 去重
engine/         # rclone rcd 编排:生命周期、unix socket、健康探针、引擎热更新
backends-go/    # rclone fork 的 Go 后端(百度/123 开放平台,✅D17;向上游 PR)
mount/          # rclone mount/nfsmount 包装(✅D16:Windows 引导安装 WinFsp)
mobile/         # V3:android / ios
docs/           # 规划文档、specs、ADR、任务卡
xtask/          # 开发工具(M0 建立)
```

跨层反向依赖需 ADR;`backends-go` 与 `engine` 之间的接入必须走统一接缝(D17:为未来转 OpenDAL 实现铺垫)。

## 桌面端验收规则(硬性,继承 partisync 2026-10-02 用户指令)

- 凡涉及桌面端 GUI 的任务,AI 必须在申请 PR 前**实际操控电脑(GUI 实操/截图验证)**确认符合预期;
  纯单测绿不算桌面端验收通过。
- 验证证据(截图/操作记录)随 PR 正文或 `docs/screenshots/` 归档。
- 无法实际操控时,PR 保持 OPEN 并显式标注「待 GUI 验证」,不得自行合入。

## 风险分级(评审深度)

- R0 纯内部有测试 → 抽审 30%;R1 跨 crate/IO 密集 → 全审;R2 钉子清单(schema/凭据/密码学)→ 双人全审。

## 版本更新简报(✅ Owner 指令 2026-10-08,强制)

- **每完成一个小版本**(Phase 关账 / V0.x 发布 / 里程碑验收),AI 必须**主动**向 Owner 提交一次更新简报:
  完成项、新增/变更决策、风险与债务台账、下一步计划、Owner 待办催办状态;
- 简报落档 `docs/reports/`(沿用 partisync 报告惯例),并在会话中直接呈现。

## 域模型术语(全域统一,权威定义见 [00-术语表](./docs/00-术语表.md),冻结前以该表为准)

- **Node(节点)**:物理存储端,涵盖本地与云端(本地磁盘、云盘账号、NAS/S3/WebDAV 端点)。代码类型 `StorageNode`。
- **Particle(粒子)**:最小数字资产单元,以内容身份为锚(同内容=同一粒,不论存于几个 Node);路径呈现为该 Particle 的一次 **Placement(驻留)**。代码类型 `Particle`/`Placement`。
- **Node 画像(NodeProfile)**:对 Node 的持续能力评估(架构 §12),策略引擎的输入。
- 引擎术语(Engine/Remote/Job/Mount/Engine Slot)与 V2+ 底座术语(Space/Device/Tag/Sidecar/Memory/Hub)见术语表 §2/§4。

## 当前阶段工作索引

- Phase 0 交付物与验收标准:[03 开发计划 §5](./docs/03-开发计划.md)。
- 架构设计(草案待评审):[06](./docs/06-架构设计-Phase0草案.md)。
- 下一步:架构评审定稿 → 《开发执行方案》固化 → SPEC/任务卡模板 → P0-WP01 起任务化。
