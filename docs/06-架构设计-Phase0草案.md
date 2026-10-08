# 架构设计(Phase 0)v1.0

> 状态:**✅ 已评审定稿(2026-10-08 Owner 审核通过,含 §12 Node 能力画像子系统与 Q1-Q6 裁决;术语按 [00-术语表](./00-术语表.md) v1.0)** | 日期:2026-10-08 | 负责人:@partiverse
> 依据:[03 开发计划 v0.5](./03-开发计划.md) · [04 rclone 选型评估](./04-rclone选型评估.md) · [05 PartiSync 协作评估](./05-PartiSync协作评估.md)
> 范围:V1(桌面三端 + rclone 引擎,与 partisync 零耦合)。V2+ 的 partisync 集成只在 §8 预留接缝,不展开。

---

## 1. 目标与非目标

**目标(V1)**:一个桌面应用,把本地磁盘、协议存储(SMB/FTP/SFTP/WebDAV/HTTP)、S3 兼容对象存储、国际主流云盘(OneDrive/GDrive/Dropbox/Box/pCloud/MEGA/iCloud-实验性)与国内开放平台盘(坚果云/123/百度)聚合为统一浏览器:统一浏览、跨源互拷、全局名称搜索、基础文件操作、凭据零明文。

**非目标(V1)**:挂载为系统盘(D16:V2);AI 语义搜索(D13:V3);多设备同步/去重(partisync,V2+);移动端(V3);任何逆向协议驱动(D2);内容缓存与推荐(合规红线)。

## 2. 总体拓扑

```
┌─────────────────────────────────────────────────────────────┐
│ Partiverse 桌面应用(Tauri 2 + React/TS,D6)                  │
│  统一浏览 · 连接管理(schema 自动生成表单)· 搜索 · 传输队列     │
├────────────── Tauri IPC(命令面,类似 specta 生成的强类型绑定)──┤
│ Partiverse 核心(Rust)                                        │
│  ① engine 协调器:rcd 生命周期/健康探针/引擎热更新(§4)          │
│  ② rc 客户端:unix socket JSON,JobManager(§3)                │
│  ③ 连接管理器:凭据→keychain,rclone config 加密注入(§5)       │
│  ④ 索引服务:SQLite+FTS5,配额预算器调度抓取(§6)               │
│  ⑤ 传输编排:rc job 包装/进度/断点/校验(§7)                    │
│  ⑥ 数据面客户端:回环 HTTP(serve http)+Range 流式(§7)         │
│  ⑦ provider 接缝 trait(§9,D17 铺垫)                          │
├─────────────────────────────────────────────────────────────┤
│ rclone rcd(sidecar,MIT,运行时下载+签名校验,独立热更新)        │
│  control plane: rc API(unix socket + 认证)                    │
│  data plane: serve http(127.0.0.1, Range/206, vfs-cache full) │
├─────────────────────────────────────────────────────────────┤
│ 各存储:本地/SMB/FTP/SFTP/WebDAV/S3 系/OneDrive/GDrive/...     │
│ backends-go(V2 起):百度/123 开放平台(rclone fork 后端)        │
└─────────────────────────────────────────────────────────────┘
```

## 3. 控制面:rc 协议约定

- **传输**:`--rc-addr unix://$DATA_DIR/engine/control.sock`(per-user 随机目录名,0700)+ `--rc-user/--rc-pass`(每次启动随机生成,只经内存传给核心;v1.74+ 默认要求认证,顺带满足)。禁止 TCP 绑定。
- **核心命令面**(白名单化封装,禁止透传任意 `core/command` 到 UI):`config/create|update|get|delete|dump`、`config/providers`(连接表单 schema 来源)、`operations/list|stat|about|copyfile|movefile|delete|mkdir|uploadfile|publiclink`、`sync/copy|move|bisync`(V2)、`job/status|list|stop`、`core/stats|version|bwlimit|quit`、`mount/*`(V2)。
- **JobManager**:所有长操作走异步 job(返回 jobid);状态机 `queued→running→done|error`;崩溃恢复 = 启动时 `job/list` 对账 + 任务表落 SQLite;超时与重试策略按 provider 分级。
- **错误处理**:rc 结构化错误(`error/input/status/path`)映射到核心错误体系(参照 partisync-core 的 Severity 分类学);`429/503` 一律交配额预算器退避,不直接抛给 UI。

## 4. 引擎生命周期(协调器职责)

1. **获取**:安装包不内置 rclone;首次启动按锁定版本从 GitHub Releases 下载,校验官方 sha256 + GPG 签名后落 `app_local_data/engine/<ver>/`(参照 rclone-ui 模式);支持"发现并采用系统 PATH 中的 rclone"(版本需满足锁定区间)。
2. **启动**:spawn `rcd`,注入 `--config <加密路径>` 与 `RCLONE_PASSWORD_COMMAND`(§5);启动超时 10s;`rc/noop`+`core/version` 探活,版本不符锁定区间则告警不阻断。
3. **健康与重挂**:每 30s 心跳;V2 挂载面额外做挂载点探活,sleep/resume 后自动 unmount+remount(partisync/社区已知坑:resume 失联)。
4. **升级**:应用内检查新版本 → 下载校验 → 新版本引擎并行验证(rcd 冒烟)→ 原子切换目录 → 旧版保留一个版本以便回滚。**引擎热更新独立于应用发版**。
5. **退出**:SIGTERM,2s 超时后 SIGKILL;退出前 `core/quit` 兜底;崩溃后由协调器重启(指数退避,最多 5 次)。

## 5. 凭据与配置安全(满足"零明文"红线)

- **config 加密**:`rclone config encryption set`(nacl secretbox);主密码 = 启动时由核心生成的随机密钥,存 OS keychain(Keychain / Credential Manager+DPAPI / libsecret);进程启动经 `RCLONE_PASSWORD_COMMAND` 注入(平台脚本封装,官方推荐模式)。
- **OAuth 流程**:应用内嵌本地回调服务器(127.0.0.1 随机端口)完成 OAuth;client_id/secret 分发策略:V1 用应用内置 client(GDrive 走 `drive.file` 常规验证,✅D7);**client_secret 属敏感项,随应用分发是桌面应用通行做法,文档明示**;用户可替换自建 client(GDrive 共享 ID 时代的要求,rclone 参数直注)。
- **token 写回**:rclone 自动把刷新 token 原子写回加密 config——核心不做 token 持久化,**凭据单一真相源 = 加密 config + keychain**。
- **连接管理 UI**:表单由 `config/providers` schema 动态生成(Type/Required/IsPassword/Advanced/Exclusive 映射);提交时经 `config/create` 注入;密码字段在 UI 与日志全程脱敏(`--use-json-log` + 日志脱敏过滤器)。
- **禁止事项**:禁用 `rclone obscure` 作为"加密";禁止任何凭据进遥测/崩溃报告。

## 6. 索引与配额预算编排

- **存储**:SQLite(索引/任务表/连接元数据缓存)+ FTS5(文件名/路径;内容搜索 V3 复用 partisync-index,✅D21)。
- **配额预算器**:每 provider 一张令牌桶(参数表:rclone 各后端限速 + 坚果云 600req/30min 等显式配置),`list/about/stat` 与索引抓取统一走预算器;预算器持久化(重启不丢桶状态)。
- **索引策略**:手动浏览不触发全量索引;用户显式"为该盘建立搜索索引"后,按 `lsjson --recursive` 分页抓取(有 delta 能力的源走 delta;rclone 侧按 `--fast-list` 权衡),断点续抓(游标落库),可暂停可限速。
- **一致性**:目录视图默认"本地缓存 + TTL 后台刷新";用户手动刷新强一致;缓存行带 `fetched_at/scope` 便于失效。

## 7. 传输与数据面

- **数据面**:`rclone serve http --addr 127.0.0.1:<随机端口>`(每引擎实例一个),预览/播放统一经此取流(Range/206 已源码实证);`--vfs-cache-mode full` 仅用于挂载与播放场景,纯预览走直读+Range,控制缓存水位。
- **跨源复制/移动**:同后端优先服务端复制(rc `operations/copyfile` 能力协商);异构走核心编排(rc job `sync/copy` 单文件语义 + 哈希校验;哈希不可比时降级 size+采样哈希——**采样哈希实现 V1 沿用简单两段采样,V2 交 partisync cas**)。
- **任务队列**:SQLite 任务表(源/目标/状态/进度/校验和/错误),UI 订阅 `core/stats` 聚合进度;失败重试与 `--retries` 对齐;限速经 `core/bwlimit` 全局+按任务两级。
- **V2 挂载编排**(✅D16):Windows 引导下载 WinFsp 官方安装器(用户手点)→ 检测就绪 → `rclone mount`(vfs-cache writes/full);macOS `nfsmount`(FUSE-T 引导同理);Linux FUSE 直用。

## 8. partisync 集成接缝预留(V2,零耦合现状)

- 核心对上层暴露 `trait AssetStore`(列目录/读写元数据/去重查询),V1 实现 = SQLite 自建;V2 增加 partisync-graph 实现(Entry/Content 模型,✅D20),UI 无感切换。
- 搜索面 `trait SearchBackend`:V1 = FTS5;V3 = partisync-index(✅D21)。
- 插件面 `trait PluginHost`:V3 = partisync-ext-host,扩充 cloud.read/write 能力位(✅D22)。
- **哈希旁路约定**(喂 partisync cas,避免为取身份下载全文件):provider 层暴露 `content_hash(alg, value)` 能力位,rclone 元数据中的可用哈希(S3=MD5/ETag 单段、各盘哈希)直通;无哈希源才走下载采样。

## 9. provider 接缝(D17 铺垫)

```rust
trait CloudSource {            // backends-go(rclone 后端)与未来 OpenDAL 实现共同实现
    fn scheme(&self) -> &'static str;               // "baidu-open" / "123pan-open"
    fn auth(&self) -> AuthFlow;                     // OAuth/扫码等,知识库描述驱动
    fn capabilities(&self) -> Caps;                 // 哈希/直链时效/配额/服务端复制/秒传
    // 数据面统一经 rclone remote 或(未来)OpenDAL operator,本 trait 只做编排骨架
}
```
- **provider 知识库**(`docs/providers/<盘>.md`,Apache-2.0):API 端点、鉴权流、限流数值、直链时效、怪癖与变更日志——Go 后端与未来 Rust 重实现共用同一知识库,转③时按库重写即可(✅D17 铺垫)。

## 10. 安全与合规核对表(V1 出厂前逐项打勾)

- [ ] 凭据零明文(加密 config + keychain;崩溃报告/日志脱敏抽查)
- [ ] rc/engine 端口仅 unix socket + 回环;外部扫描零暴露
- [ ] 引擎下载强校验(sha256+签名)
- [ ] 红线自查:无逆向驱动、无内容缓存/推荐、无"搬运/加速"话术(02 §4.4)
- [ ] 隐私政策草案与实际数据流一致(待 Phase 0 产出)
- [ ] WinFsp 引导文案法务复核通过(✅D16 前提)

## 11. 开放问题裁决(2026-10-08 Owner 裁决,全部关闭)

| # | 问题 | 裁决 |
|---|---|---|
| Q1 | 引擎版本锁定策略 | **固定 minor(如 1.75.x),安全补丁(patch)自动跟进** |
| Q2 | GDrive `drive.file` 限制的过渡 | **V1 明示限制 + 引导用户自建 client**(共享 client_id 已停用,自建=独享配额) |
| Q3 | SQLite 索引库加密 | **V1 不加密**(凭据才敏感;索引明文本地可接受),M0 出 ADR 记录 |
| Q4 | 引擎实例数 | **单实例 + remote 前缀隔离** |
| Q5 | 多 rcd 准备 | **单实例,但协调器按"引擎槽位"设计**(remote 命名空间、数据面端口、cache-dir 全部参数化),为多 rcd 预留实现位——**候选 Pro 专属能力**(重负载独立引擎/工作区隔离),付费墙设计时纳入 |
| Q6 | Tauri IPC 命令面粒度(原表 Q5) | **细命令 + specta 强类型绑定**(Owner:"其他问题全部同意") |

---

## 12. Node 能力画像与智能策略子系统(Owner 指令,2026-10-08;术语按 [00-术语表](./00-术语表.md) v1)

**术语**:物理存储端(本地磁盘/云盘账号/NAS/S3/WebDAV 端点)统称 **Node**;数字资产的原子单元称 **Particle**(以内容身份为锚,一粒可多驻留)。本子系统的评估对象是 **Node**;画像让用户无需理解任何一家盘的 API 差异、限流、直链时效、性能怪癖——策略引擎把这些差异吸收为**无感的体验**,让用户对自己的数字资产始终有**掌控感**。

**产品意图**:该快的快、该让的让、该提醒的才提醒;画像仅用于改善体验,不做跨用户数据汇总(隐私红线)。

### 12.1 画像数据模型(SQLite `node_profile`)

- **static_caps**(静态能力,来自 `operations/fsinfo` + `backend/features` + provider 知识库):哈希类型与算法、mtime 精度、大小写敏感、原子改名/移动、服务端复制、quota(about)可用性、分页上限、直链时效等级、单文件/分片上限、鉴权类型与 token 有效期。
- **dynamic**(动态,滚动窗口):健康态(healthy/degraded/down)、限流信号(429/503 频率)、性能统计(list P95、吞吐、延迟——**优先从真实流量被动测量**)、配额水位、直链实测平均时效。
- **meta**:last_probe、置信度、画像 schema 版本;全部本地存储,不做跨用户汇总(隐私红线)。

### 12.2 持续评估(两级,全部受配额预算器约束)

1. **被动优先(零额外成本)**:正常浏览/传输/索引的流量即测量源——延迟、吞吐、错误率、限流信号随手沉淀。
2. **主动探针(小而稀)**:连接时轻探(一次 list + 小文件 head);建索引/大传输前预探;深度性能探针(`rclone test speed` 类)仅周级或用户显式触发;每次探针消耗预算器令牌并记录,绝不阻塞用户操作。

### 12.3 智能策略目录(策略引擎消费画像)

| 领域 | 策略(对用户无感) |
|---|---|
| 传输 | 多副本自动选路(同一 Particle 择优 Node 驻留);忙时自动降速/闲时加速;限流自适应退避;直链时效感知的重取策略 |
| 浏览 | 按 Node 性能分级设置缓存 TTL 与预取深度;慢源"先出缓存壳、渐进加载";健康态徽标静默降级 |
| 索引 | 刷新节奏 = f(变更率 × 配额 × 性能),慢源/紧配额源自动降频 |
| 挂载(V2) | 按 Node 分级选 `vfs-cache-mode` 与 read-chunk 参数 |
| 掌控面板 | 总空间水位、各 Node 健康与性能、异常清单(token 将过期/配额将满/接口漂移)、去重潜力(同 Particle 多驻留);**重要异常才打扰,其余收敛为红点** |
| V3 联动 | 画像参与 AI 整理建议(如"该 Node 快满了,建议把冷粒迁到 X") |

### 12.4 分期落地

- **V1**:static_caps + 被动健康 + 预算器联动 + 掌控面板最小版(空间/健康/异常)。
- **V2**:性能画像 + 传输自动选路 + 挂载参数分级。
- **V3**:与 AI 语义层联动(画像驱动的整理/迁移建议)。
