# M1 · partisync 复用评估(代码级盘点)

> 状态:v1.0(2026-10-08,基于 partisync @ d7f73ff 只读盘点) | 对应决策:D19-D22
> 结论先行:**桌面壳采用"方案 b"(搬 src-tauri 骨架 + React 按交互稿重写);V1 唯一代码复用例外 = core 的 caps.rs+error.rs(拷贝 ~200 行);采样哈希 cas 无现成实现,自写;其余按原计划 V2/V3 引入。**

## 1. 桌面壳:方案对比与裁定

| 方案 | 做法 | 工作量 | 裁定 |
|---|---|---|---|
| (a) 沿用 vanilla JS 壳改造 | 摘除记忆/扩展 tab,数据源换 rc | 最小(~1-2 周) | **否决**:与 D6(React)冲突;1053 行单文件 JS 已现维护劣化;V2 多 Node/传输队列状态复杂度失控 |
| **(b) 保留 src-tauri 骨架 + React 重写 UI** | 搬配置/错误形状/窗口状态/子进程管理模式;UI 按 M10 任务卡移植;纯逻辑照抄 | 中(~2-4 周当量) | **✅ 采用**——这正是"适当采用"的准确含义,降低 M10 沉没损失 |
| (c) 仅作交互参考从零建壳 | 只读任务卡/截图 | 最大增量 | 作为 (b) 的兜底 |

**方案 (b) 的搬运清单(具体到文件)**:
- 直接搬:`tauri.conf.json`(CSP `script-src 'self'` 生产配置、capabilities、窗口 1200×800——含两条实战教训:CSP 拦 inline onsubmit 导致整页 reload;dev 模式 HMR 需放宽 script-src)、`error.rs`(`{kind,msg}` 统一 IPC 错误形状)、`window_state.rs`(256 行纯函数+6 单测,逻辑像素/损坏回落)、`lib.rs` 的 CLI/bench 钩子模式;
- **改造复用**:`mcp_sidecar.rs`(364 行)→ rclone sidecar 管理器:懒 spawn、current_exe 兄弟路径解析、stdio JSON-RPC 路由、EOF 自动重启、kill_on_drop、argv 数组防注入——rclone 场景协议更简单(无 handshake/_meta 包袱),但生命周期模式完全同构;
- **照抄纯逻辑**(app-core-v3.js 中约 30-40%):sizeFmt/timeFmt/relTime、三态排序循环(含 aria-sort/目录恒前排)、面包屑、指纹色黄金角散列、snippetHtml 高亮协议(sentinel 转义次序不变量)、过滤 chips 纯函数、结果-详情联动的并发守卫+可见性门控+"点击不清空结果"硬约束(约 60 行行为规格);
- **React 重写**:7 个 view 的 DOM 层(其中记忆/扩展 tab 纯 partisync 特有,不移植;浏览/检索/作业为通用文件管理交互);ui_hardening 静态探针体系(1248 行 JS 字符串断言)作废,改 ESLint 规则重建(~1-2 天);
- **交互规格红利**:partisync `docs/tasks/M10-WP01..03` 任务卡是现成 PRD(根因/硬约束/验收+全 7 tab 巡检截图)——按稿移植条件极好。

## 2. V1(M1)代码复用裁定表

| 件 | 裁定 | 说明 |
|---|---|---|
| partisync-core 的 `caps.rs`+`error.rs` | **✅ V1 拷贝引入(~200 行,Apache-2.0,标注来源)** | HashCaps.intersect + `change_detection_needs` 与 rclone 哈希能力协商**直接同构**;Severity 分类即 WP03 错误映射骨架。拷贝优于 git dep(保持 M1 零依赖,partisync 0.x 漂移隔离);Ulid/Hlc 闲置不拷 |
| partisync-cas | **✗ V1 不引入**;自写采样哈希 | cas 拖 sqlx/fjall/RS 全家(2347 行)为 60 行哈希不值;**全仓无两段采样哈希先例**(partisync 的答案是下载后全量 BLAKE3)→ M1 自写 blake3(头N+尾N+size)约 30-50 行,与全量哈希分字段存放 |
| graph 的 RateLimiter(令牌桶) | 仅参考/拷贝 35 行;或用 governor crate | 若限速下沉给 rclone(`--bwlimit`/`core/bwlimit`)则此件省却——**WP03 设计时定分工,倾向下沉 rclone、预算器只做调度** |
| graph 的 jobs.rs / indexer.rs | 仅参考设计 | 五态状态机/checkpoint 字典序语义写进 WP03/WP08 设计注释;代码强耦合不搬 |
| cli 的 s3api.rs 伪 S3 服务端 + M1-WP02 互操作报告 | **WP11 测试资产** | 本地伪 S3(path-style/MPU/ETag=MD5)+ rclone v1.75.1 全绿矩阵 + 5 个踩坑(HEAD 目录 404、RFC1123、CreateBucket 语义等)——provider 验证环境现成素材 |
| desktop 的 tests/commands.rs 范式 | ✅ 方法论复用 | `tauri::test::mock_builder` 直接调 command 函数绕过 webview 的测试范式 |
| 其余(provider/sync/index/ai/ext-host/gateway/hub) | V2/V3 按原计划(D19-D22) | 无提前引入 |

## 3. 事实纠偏(记录防再犯)

1. **partisync 全仓无 rclone 客户端代码**(存储引擎=OpenDAL;rclone 仅出现在互操作测试与文档)→ M1 的 WP02/WP03/WP06 对 partisync **严格零复用**;
2. **采样哈希无先例可抄**——M1 自写并配 proptest;
3. partisync 对 mtime 缺失的口径"置 0 + caps 标注"可直接继承进 Node 画像(WP09)。
