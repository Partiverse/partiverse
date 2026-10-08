# PartiSync 协作评估:Partiverse × PartiSync 深度协同方案

> 项目代号:Partiverse(暂定)| 日期:2026-10-08
> 评估对象:[Partiverse/partisync](https://github.com/Partiverse/partisync)(本地克隆核查 @ `d7f73ff`,2026-10-08)
> 前置文档:[市场调研](./01-市场调研报告.md) · [可行性分析](./02-可行性分析报告.md) · [开发计划 v0.3](./03-开发计划.md) · [rclone 选型评估](./04-rclone选型评估.md)
>
> **✅ 决策确认(2026-10-08,用户拍板 D19)**:两个产品互补分工——底座做底座的事,聚合做聚合的事,合并后即最终产品;必要时移除 PartiSync 桌面 GUI、保留 CLI,为 Partiverse 让路。执行含义与 GUI 撤退预案见 §9。

---

## 1. 结论(TL;DR)

**PartiSync 与 Partiverse 是天然的「引擎-产品」互补关系,协作价值极高;但两仓引擎栈不同(rclone vs OpenDAL 自研),必须用明确的接缝设计而不是含糊的"整合"。**

1. **PartiSync 不是竞品,是另一半**:它自我定位就是「基建底座,UI 是壳不是核」(蓝图 §1.3),M0-M9 全部关账、v0.1.0-beta 已签名发布(2026-10-05),约 7.9 万行 Rust、17 个 crate、468 次提交,今天仍在提交。它已经建成的恰好是 Partiverse 计划里**最贵、最不属于"多云浏览器"差异化**的部分:CAS 去重、双向同步与对账、设备配对与 E2EE、混合检索(BM25+向量+重排+OCR/转写)、WASM 扩展宿主、MCP 网关、Hub 多端。
2. **Partiverse 的差异化空间不受损**:云端 OAuth 账号管理、中国网盘开放平台接入、跨云传输编排、消费级浏览/预览/搜索 UX——这些 PartiSync **明确没有且已登记为缺口**(OAuth 自 M1 起悬置至今、provider 凭据明文、无跨云编排)。两个产品的边界天然清晰。
3. **引擎栈张力必须裁决**:Partiverse v0.2 选 rclone,PartiSync 选 OpenDAL(实际只启用 fs/s3/webdav 三种)。**推荐双引擎分工而非合并**:rclone 管"云协议广度"(65+ 后端+OAuth 流+挂载),PartiSync 管"本地资产智能"(CAS/同步/检索/AI/多端);接缝已被双向验证——partisync 可消费 rclone `serve webdav` 的输出(它有 WebDAV provider),rclone 可把 partisync 挂为 S3 remote(这是 partisync M1 的头号验收项,每夜 CI 在跑)。
4. **复用方式首选 crate 级依赖而非进程级**:PartiSync 各 crate 都是 Apache-2.0 的库(graph/cas/index/ai 可进程内嵌入 Partiverse 的 Rust 核心),比走 MCP/守护进程耦合更简单;但必须加隔离适配层(partisync 处于 0.x,三周内 schema v1→v18,API 漂移快)。
5. **流程复用同样重要**:PartiSync 的规格驱动纪律(十条铁律、Task-ID 全程追溯、≤400 行原子 PR、AI 对抗审查+人工终审、变异测试、GUI 截图验收)已在 79k 行规模上验证可行——Partiverse 应直接继承这套方法论,而非另起炉灶。

---

## 2. PartiSync 事实卡(核查于 2026-10-08)

| 维度 | 事实 |
|---|---|
| 定位 | 「AI 时代数字资产传输与多端融合管理基建底座」——对标空白带:rclone 协议广度 + rsync delta + Spacedrive 融合建模 + Tectonic 元数据规律 + AI 原生(蓝图 §2.7) |
| 形态 | 无界面核心(设计上 `partisd` 守护进程)+ CLI + Tauri 2 桌面壳 + 可选自托管 Hub 集群;**"UI 是壳不是核"** |
| 成熟度 | M0-M9 全部关账,v0.1.0-beta 已签名发布(2026-10-05);M10 进行中(主题:前端 GUI 加速,用户拍板) |
| 规模 | 17 crate、约 79,000 行 Rust、216 文件、inline 测试 134 + 集成测试 429 + proptest 22;变异分数 82.4%(M0) |
| 许可 | Apache-2.0(与 Partiverse 全兼容,代码可自由共享) |
| 桌面栈 | Tauri 2(ADR-0024)+ 原生 vanilla JS 前端(7 个 tab:浏览/检索/记忆/同步/重复内容/扩展/作业),12 个 IPC command |
| 发布 | GPG/ed25519 签名发布工具链(ADR-0027/0030),v0.1.0-beta 验签 PASS |

**已完成能力**(逐 crate 核实):
- **CAS**(3,625 行,成熟):blake3 内容身份、fastcdc 分块、引用计数去重(ChunkStore put/get/incr/decr)、RS(10,4) 纠删、pack 打包、NVMe→HDD→S3 分层、分代 GC。
- **同步引擎**(7,303 行,成熟):域分离 oplog(HLC)+ bisync 不动点 + 冲突保留两者带血缘 + max-delete 熔断 + 4/8/12-bit 分级 Merkle 对账;BIP-39 助记词设备配对(Ed25519/X25519);空间级 E2EE(Argon2id→space→content,XChaCha20-Poly1305)。
- **检索**(4,343 行):tantivy BM25(文件名/标签/**OCR/转写文本**多字段)+ usearch HNSW(BGE-M3 768d 文本、CLIP 512d 图像)+ RRF 融合 + bge-reranker 可选精排;合成库 nDCG@10=0.754;带 eval 基准框架。
- **AI sidecar 管线**(4,207 行):缩略图/EXIF/OCR/转写/嵌入/C2PA 六级流水线,挂 content 不挂 entry(N entry→1 content 只算一次),持久作业系统可崩溃恢复,模型钉版管理。
- **MCP 网关**:11 个工具(asset_search/asset_read/asset_organize/dataset_export/job_status/memory_write/search/verify/update/delete/ext_list);stdio + Streamable HTTP(TLS + JWT RS 验签 + RFC 9728 PRM + fail-closed 授权矩阵,M10-WP05 刚关账)。
- **Hub**(16,212 行,最大 crate):fjall 分片元数据 + openraft + iroh 中继(QUIC 打洞,BLAKE3 验证流)+ 配额/审计/ACL。
- **WASM 扩展宿主**(2,591 行):wasmtime WIT Component,"扩展即 MCP 工具",minisign 装载期强制验签,能力白名单(一期仅 index.read/clock.read),epoch/fuel 防失控。
- **FUSE**(2,582 行):fuser,mountpoint-s3 式诚实语义(随机读+新文件顺序写),WAL 写回 overlay + 崩溃恢复(proptest 1024 例),`/by-hash` CAS 视图;**仅 Linux 实测**。
- **对外协议服务**:S3/WebDAV 服务端(CLI `ui --s3/--dav`),**rclone v1.75.1 全矩阵互操作绿**(含 bisync 双向变更注入收敛),每夜 CI(`scripts/interop.sh`)。

**明确缺口**(Partiverse 不能假设存在的能力):
- **无云端 OAuth/账号管理**:M1-WP00 登记"OAuth 归 M1 后半",至今未做;provider 凭据为明文 config 参数(S3 缺省 key 还写死 "partisync");仅 fs/s3/webdav 三种 scheme。
- **`partisd` 守护进程是 8 行骨架**:实际消费方式是桌面进程内直开 SQLite 库,或 spawn `partisync-mcp` 子进程走 stdio JSON-RPC。
- **无跨云传输编排**(等价物=网关+rclone bisync 外部组合);无配额预算器(仅 graph 层 bwlimit 令牌桶)。
- 语义向量检索被 M10-WP04 **正式拍板"条件不成熟暂不实施"**;桌面检索存在 sidecar 持 bm25 写锁的已知债(LockBusy);M10-WP01 正在修"GUI 搜索形同虚设"。
- FUSE 仅 Linux 验证;S3 网关 SigV4 验签未做(接受任意凭证,仅限本机);真实 IdP e2e、移动端、公网部署均为条件触发项。

---

## 3. 能力对照:Partiverse 计划功能 vs PartiSync 现状

| Partiverse 计划功能 | PartiSync 现状 | 协作判定 |
|---|---|---|
| 统一浏览/多源聚合(V1) | remote_index 可把 S3/WebDAV 拉入图谱;无 OAuth、无网盘 | **Partiverse 自建(核心差异化)** |
| 云盘 OAuth 账号管理(V1) | 无,且已悬置 | **Partiverse 自建**(配 rclone OAuth 引擎) |
| 全局名称搜索(V1,FTS5) | graph 有 FTS5 trigram(memory);tantivy BM25 更强 | V1 用自带 SQLite+FTS5(轻、独立);V2 起评估切 partisync-index |
| 重复文件检测(V2) | **DupGroup API + CAS 采样/全量哈希 + 桌面"重复内容"tab 已完成** | **直接复用**(crate 级) |
| 跨云传输(V2) | 无编排;有 ChunkPlan delta 与传输 trait 骨架 | **Partiverse 自建编排**,底层可调 partisync-transfer/rclone |
| 挂载(V2) | partisync-fuse 只读+受限写、仅 Linux;rclone mount 全平台方案更全 | **rclone mount 为主**;本地 CAS 空间用 partisync-fuse |
| 流媒体播放(V2) | 无专门面;S3/WebDAV 网关可透传 Range | Partiverse 自建(rclone serve http 数据面) |
| 备份/同步任务(V3) | **bisync+oplog+Merkle 对账成熟**(且与 rclone bisync 语义对齐) | **复用**(crate 级),UI 包状态机 |
| AI 语义搜索(V3) | **tantivy+usearch+RRF+reranker+OCR/转写 sidecar 全套已建成**(向量档被拍板缓发) | **复用**(crate 级)——省掉 V3 最大一块自研 |
| 自然语言整理(V3) | MCP asset_organize + Preview→Commit→Verify 事务设计 | **复用接缝**(MCP 工具面) |
| E2EE 保险箱(V3) | **空间级 E2EE 已实现**(XChaCha20-Poly1305,密钥层次完整) | **复用** |
| 插件/驱动市场(V3) | **WASM 扩展宿主+minisign 验签已建成**(能力白名单待扩) | **复用**,需扩展 cloud.write 类能力位 |
| 多设备/多端(V3+) | iroh 配对+E2EE+Hub 全套 | **复用**(Partiverse 计划里原本没有这块) |
| 移动端(V3) | 未做(Tauri 2 移动待启动) | 共同待建 |

**结论:Partiverse V3 清单里约一半条目(去重/同步/E2EE/AI 检索/扩展/多端)在 PartiSync 已有成熟或接近成熟的实现。以 crate 复用计,Partiverse V3 的自研量可再砍 40-50%。**

---

## 4. 引擎栈张力与消解方案

**张力**:Partiverse v0.2 引擎 = rclone(Go sidecar,65+ 后端,OAuth,挂载,serve);PartiSync 引擎 = OpenDAL(Rust 库,仅 3 service,无 OAuth)。若各建各的,会出现**两个凭据库、两套缓存、两份索引**;更糟的是 partisync 的 remote_index 目前靠"下载后算 BLAKE3"取内容身份——对云端大文件代价极高,必须有喂哈希的通道。

**消解:双引擎 + 一条已验证的接缝 + 哈希旁路**

```
┌────────────────────────────────────────────────────────────┐
│              Partiverse(Tauri 2 桌面产品,脸面)              │
│   统一浏览 · 账号管理 · 传输编排 · 搜索/整理 UX · 付费墙        │
├─────────────引擎接缝(需 ADR 定义)──────────────────────────┤
│ ① rclone rcd sidecar:云协议广度/OAuth/挂载/serve 数据面      │
│    (V1 主力,维持 D15 不变)                                  │
│ ② partisync crates 库内嵌:graph(图谱/去重)/cas/sync/       │
│    index+ai(检索)/ext-host(V2 起逐步引入,隔离适配层)        │
│ ③ 哈希旁路:Partiverse 从 rclone 元数据取可用哈希            │
│    (S3=MD5 等)喂给 partisync cas,避免为取身份而整文件下载;  │
│    无哈希源才走下载采样(partisync 两段式采样哈希已支持)       │
└────────────────────────────────────────────────────────────┘
互操作兜底(双向已验证):rclone serve webdav → partisync WebDAV provider;
partisync S3/WebDAV 网关 → rclone mount/bisync(每夜 CI)。
```

**为什么 V1 仍坚持 rclone 不变**:PartiSync provider 无 OAuth、仅 3 scheme、无挂载广度,V1 若等它补齐,MVP 遥遥无期;而 rclone 侧 OAuth client 注入、挂载、serve 都是现成的。**V1 两个项目零耦合,互不阻塞。**

**为什么 V2 起引入 partisync**:去重/同步/AI 检索这些功能进入需求点时,partisync crates 以 git 依赖(pin tag/rev)+ 隔离适配层(`trait AssetGraph`/`trait SearchBackend`)接入——partisync 是 0.x、schema 三周迭代 18 版,适配层是防火墙,升级走 CI 演练(它自己的风险矩阵第 3 条就是这么设计的)。

**国内盘后端的新选项(D17 修订)**:原方案是 fork rclone 写 Go 后端。现在出现第三条路:**把百度/123 开放平台写成 OpenDAL service(Rust)**——一份代码同时服务 Partiverse(直接消费)与 PartiSync(provider 层直接可用),且 OpenDAL 是 Apache 顶级项目、接受 service 贡献,治理与语言栈都与两个项目一致。代价是失去 rclone 生态的分发面(挂载这些盘仍需 partisync 网关+rclone mount 组合)。此权衡列入 D17 备选。

---

## 5. 推荐协作模型(三层)

**第一层:产品/品牌——「Partiverse 套件」双产品线**
- **Partiverse Browser**(本项目):多云聚合文件浏览器,消费者人脸,差异化=账号管理+跨云+搜索整理 UX+中国盘。
- **PartiSync**(已有):本地资产智能底座(去重/同步/E2EE/AI/Hub),面向极客与自托管,CLI+桌面壳。
- GitHub 组织已同名(Partiverse),品牌天然统一;对外口径:Partiverse Browser 是"把 PartiSync 的资产智能带到所有云端"的产品。互不阻塞、独立发布、共享组件。

**第二层:代码——crate 级复用(带隔离适配层)**
- V1:零依赖。仅共享流程与工具链约定。
- V2:引入 `partisync-graph`(去重 DupGroup、Entry/Content 模型)+ `partisync-cas`(内容身份与哈希旁路)。
- V3:引入 `partisync-index`+`partisync-ai`(混合检索与 sidecar 管线)、`partisync-sync`(备份/同步任务)、`partisync-ext-host`(插件市场)、`partisync-gateway`(把 Partiverse 聚合面也暴露成 MCP 工具,对齐两仓的 Agent 战略)。
- 全程 Apache-2.0,无许可障碍;所有引入走 ADR(学 partisync 的"无聊依赖"铁律)。

**第三层:流程——继承规格驱动方法论(✅ 2026-10-08 用户确认:开发方式沿用 partisync 模式)**
- 直接采用 Partiverse 版的 AGENTS.md(十条铁律、Task-ID 追溯、≤400 行原子 PR、AI 对抗审查+人工终审、GUI 截图验收、变异测试、cargo deny)。
- 这套流程在 79k 行规模上被验证过(M0-M9 全关账、35/35 PR CI 绿、追溯链抽查 PASS),是用户已跑通的纯 AI 开发范式——**Partiverse 从第 0 天就用,不要等产品变大再补**。

---

## 6. 风险与对策

| # | 风险 | 对策 |
|---|---|---|
| 1 | 引擎双栈复杂度(两套凭据/缓存/索引) | 接缝 ADR 明确:rclone 只做协议面,资产真相在 partisync graph;哈希旁路避免双下载;凭据仍只有一处(rclone config 加密+keychain,partisync provider 配置由 Partiverse 代填) |
| 2 | partisync 0.x API/schema 漂移(schema v1→v18/三周) | git 依赖 pin rev/tag + 隔离适配 trait + 升级演练 CI;复用从 V2 才开始,V1 零耦合 |
| 3 | partisync 自身缺口被误当可用 | 本文档 §2 缺口清单作为集成前置检查表;OAuth/凭据链明确归 Partiverse 建 |
| 4 | 单人 Owner 跨两仓带宽(决策/账号/法务/GUI 终审) | 发布节奏错峰(partisync beta 已发,M10 收口后再启 Partiverse Phase 1);GUI 验收规则延续"截图归档+人工终审" |
| 5 | 范围膨胀(把 partisync 的 Hub/万亿级叙事拖进 Partiverse) | Partiverse 非目标沿用 partisync §1.3 纪律:不碰 Hub 集群、不碰 POSIX 全语义;只消费 crates |
| 6 | 两仓 rust-toolchain/依赖漂移 | workspace 工具链版本对齐(双方都有 toolchain.toml 锁定);deny.toml 基线互认 |

---

## 7. 对开发计划的修订(同步至 v0.3)

1. V1(2026-11~12):**不变**(Tauri + rclone,零 partisync 耦合);新增"第 0 天采用规格驱动流程(AGENTS.md 移植)"。
2. V2(2027-01~03):重复文件检测 → 评估直接引入 partisync-graph/cas(替代自建);Windows 挂载按 D16(rclone mount+WinFsp);新增哈希旁路设计任务。
3. V3(2027-04~07):AI 语义搜索 → 复用 partisync-index/ai(自研量大幅下降);备份/同步 → 复用 partisync-sync bisync;插件市场 → 复用 ext-host;E2EE → 复用空间级 E2EE;多设备(PartiSync iroh)进入 V3+ 候选。
4. 决策清单新增 **D19-D22**(见开发计划 §9);**D17 扩充** OpenDAL service 路线为第三选项。

---

## 8. 新增待确认决策项

| # | 事项 | 推荐基线 | 说明 |
|---|---|---|---|
| D19 | 与 PartiSync 的协作模式 | **双产品线 + crate 级复用(V2 起)+ 流程继承**;备选:Partiverse 直接成为 partisd 的官方桌面壳(深耦合,重构 V1 计划)/完全独立(放弃复用) | 影响架构与排期,需最先确认 |
| D20 | 数据模型对齐 | Partiverse 的云源索引自建轻量模型;V2 引入 partisync 时采用其 Entry/Content 分离模型(去重锚点),不反向改造 partisync | 决定 V2 数据库设计 |
| D21 | AI 检索复用 | V3 语义检索复用 partisync-index+ai(tantivy+usearch+BGE-M3/CLIP+OCR/转写 sidecar),不自建;其"向量档暂缓"的拍板与 Partiverse V3 节奏对齐 | 影响 V3 工作量与模型分发策略 |
| D22 | 插件体系 | 复用 partisync-ext-host(WASM WIT + minisign 验签 + 能力白名单),Partiverse 扩充 cloud.read/write 能力位 | 替代原"驱动热更新"自研方案 |

---

## 9. 已确认决策与执行含义(2026-10-08 用户拍板)

### 9.1 确认内容(D19 ✅)

两个产品互补分工:**底座做好底座的事情,聚合做好聚合的事情,两者合并是好的产品**;必要时移除 PartiSync 桌面 GUI、只保留 CLI,为 Partiverse 让路。

### 9.2 分工边界(由此固化)

| | PartiSync(底座)只做 | Partiverse(聚合)只做 |
|---|---|---|
| 资产 | CAS/去重/内容身份、Entry/Content 图谱、同步对账、E2EE、多端(iroh/Hub) | 云盘账号管理(OAuth)、中国网盘接入、跨云传输编排、配额预算 |
| 智能 | 混合检索引擎、AI sidecar 管线、记忆层、MCP 工具面 | 搜索/整理的产品化 UX、自然语言交互、付费墙 |
| 形态 | CLI + 库(crates)+ MCP(自动化/AI 代理面,**永久保留**) | 桌面/移动 App(唯一的消费者 GUI,**唯一的脸**) |
| 发布 | 引擎节奏,无 UI 发布压力 | 产品节奏,面向用户与市场 |

### 9.3 PartiSync 桌面 GUI 撤退预案(三步,推荐节奏)

1. **立即冻结(不再新增 GUI 投入)**:`partisync-desktop` 停止新功能开发,仅保必需维护。**注意时点冲突:PartiSync 当前里程碑 M10 的主题恰是"加强前端 GUI 功能实现"**——进行中的 GUI 任务(M10-WP01 的 T04 过滤 chips/T05 空态引导、M10-WP02 的 T03)建议由 Owner 裁决止损:要么最小收尾后冻结,要么把 M10 剩余容量转投引擎面(**首推:把 8 行骨架的 `partisd` 守护进程实体化**——GUI 移除后 partisd 就是底座的常驻形态,也恰好是 Partiverse V2 集成所需的接缝)。
2. **保留为参考实现(冻结≠删除)**:现有桌面壳(7 tab/12 个 IPC command)是 Partiverse V1 UI 的需求规格与交互参照;GUI 截图验收规则过渡期改为"CLI+测试为主,Partiverse 接管 GUI 验收"。
3. **正式移除(条件触发)**:Partiverse V2 完成对 graph/cas 的 crate 级集成并可用后,在 partisync 下一个版本废弃 `partisync-desktop`(发布迁移说明:GUI 由 Partiverse 接替),仓库只留 CLI/MCP/库。不建议现在就删——M10 已关账的桌面成果(WP03 桌面深耕二期)还有参考价值。

### 9.4 "合并是好的产品"的路线含义

- V1(2026-11~12):Partiverse 独立产品(rclone 引擎,零耦合),partisync 同步冻结 GUI、(建议)实体化 partisd——**两条线互不阻塞**。
- V2(2027-01~03):Partiverse 引入 partisync crates(去重/CAS),成为"底座之上的第一个合并场景";同窗内 partisync-desktop 废弃。
- V3(2027-04~07):合并深化——检索/AI/同步/插件/E2EE 全面复用底座,Partiverse 成为底座的官方 GUI;对外口径从"两个产品"过渡为"**Partiverse,由 PartiSync 引擎驱动**"。
- D20-D22(数据模型/AI 检索复用/插件体系复用)是 D19 的下游细化,推荐基线随 D19 确认而强化,仍待逐项确认。
- **V4(✅Owner 定向 2026-10-08):桌面深度期**——partisync 能力的最直接产品化体现就在 Partiverse:多引擎槽位/媒体中心/NAS 远程引擎(partisd on NAS)共同构成"较为完整展示 partisync 能力"的舞台。

### 9.5 能力边界宪法(✅Owner 指令 2026-10-08)

**PartiSync 有成为通用解决方案的野心;Partiverse 是其能力的最直接产品化体现,但不得让它沦为附庸。** 三条裁定:

1. **不代劳**:凡属 partisync 定位内的能力(CAS/去重/内容身份、同步对账、混合检索与 AI 管线、E2EE、WASM 扩展宿主、记忆层),Partiverse **一律消费 partisync crates,不得在自身核心另造平行实现**;partisync 暂缺的能力(如采样哈希),优先以"需求建议"推动 partisync 提供,短期过渡件必须标注「过渡性,迁移目标=partisync 能力 X」。
2. **Partiverse 的永久自有域**(不属于 partisync 定位,自研正当):多云引擎编排(rclone)、云盘账号管理、跨 Node 传输编排、配额预算调度、Node 画像、全部 UI/UX 与商业化。
3. **建议回流机制**:Partiverse 对 partisync 的优化/深化需求,以《partisync 需求建议》清单形式提交 Owner 转交 partisync 仓库(走其 SPEC 流程),不直改其代码、不 fork 分叉;每期简报同步建议清单状态。

**当前建议清单(v1,2026-10-08)**:
| # | 建议 | 动机(Partiverse 侧) | 期许时点 |
|---|---|---|---|
| S1 | **partisd 守护进程实体化**(现为 8 行骨架) | V4 远程引擎(NAS 常驻中枢)的前提;GUI 撤退后底座的常驻形态 | 尽快(替换 partisync M10 GUI 容量) |
| S2 | **采样哈希能力**(head+tail+size) | M1 大文件校验;cas 现仅全量 BLAKE3 | M1 前后均可,短期 Partiverse 自写过渡件 |
| S3 | **provider 哈希旁路接口**(摄入外部哈希而非下载计算) | 云端大文件取内容身份避免整文件下载 | V2 引入 cas 时 |
| S4 | **向量检索解锁**(解除 M10-WP04"暂缓"拍板) | Partiverse V3 AI 语义搜索(✅D21) | M3 前 |
| S5 | **移动端路线**(iroh/sync 的移动承载) | Partiverse V3 移动端共享底座 | M3 前 |
