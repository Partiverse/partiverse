# Provider 知识库 · 百度网盘开放平台(baidu)

> 信息核实日:**2026-10-08**(官方文档实时抓取) | 维护:每次接入变更后更新 | 许可:Apache-2.0
> 用途:Partiverse 接入 baidu-open 的唯一事实源;Go 后端(rclone fork)与未来 OpenDAL 实现共用(✅D17)。
> ⚠️ 未能核实项在文中显式标注;官方未公开统一 QPS 数值表。

## 1. 接入条件

- 入口:https://pan.baidu.com/union | 文档:https://pan.baidu.com/union/doc/
- 凭据四件套:AppID / AppKey / SecretKey / SignKey;回调地址在控制台配置(可多个,**公网地址,配置约 1 小时生效**)
- **应用用途双轨(2026-08-03 起,关键)**:
  - **个人使用**(≤10 人):**全程免审,创建即"已上线"** → Partiverse 开发/内测通道
  - **公开发布**:创建后"开发中",须提交上线审核(材料:名称/描述≥30 字/类别/测试账号/**申请开通的 API 清单及必要性说明(过审仅开通所列接口)**/演示视频;2025-07-15 起生效)→ Partiverse 分发前通道
- 未审核态限制:10 次/小时、10 个授权用户
- 个人实名:1 个应用,身份证+邮箱+手机,**立即可审**

## 2. 鉴权(OAuth2)

| 项 | 值 |
|---|---|
| 授权端点 | `GET http://openapi.baidu.com/oauth/2.0/authorize?response_type=code&client_id=...&redirect_uri=...&scope=basic,netdisk`(scope 固定;支持 qrcode 扫码/display/state/force_login/device_id) |
| code | 一次性,**10 分钟过期** |
| token 端点 | `GET https://openapi.baidu.com/oauth/2.0/token?grant_type=authorization_code&code=...&client_id=...&client_secret=...&redirect_uri=...` |
| access_token | **30 天**,可刷新,刷新后仍 30 天 |
| refresh_token | **10 年,一次性**:使用后即失效,必须保存刷新响应中的新 refresh_token;刷新失败旧 token 也失效 → 重新授权 |
| 简化模式(oob) | 无公网回调的应用可用 `redirect_uri=oob` 取码;Implicit token 30 天**不可刷新** |
| 坑 | 第二次申请 token 会使第一次 access_token 失效;**token 过期错误码=20016**(无效 20017;旧文档"111=token 过期"已不适用,111 现义=有异步任务执行中) |

**Partiverse 集成注意**:百度要求公网回调 → 桌面端采用"授权页展示 code + 用户复制回填"(oob 式),或我方静态授权中转页仅回显 code(**code 换 token 必须在本地完成**,凭据零过服务器红线)。

## 3. 核心接口(域 `https://pan.baidu.com/rest/2.0`)

| 能力 | 端点 | 要点 |
|---|---|---|
| 目录列表 | `GET /xpan/file?method=list` | dir/order(name\|time\|size)/desc/start/limit(默认 1000,≤1000)/web/folder;**非递归** |
| 递归列表 | `GET /xpan/multimedia?method=listall` | **path 仅支持 /apps/{appname}**;cursor 分页(limit≤10000);ctime/mtime 增量;**官方建议 ≤8-10 次/分钟(31034 频控)** |
| 关键词搜索 | `GET /xpan/file?method=search` | key≤30 字符;category 1-7;**num 固定 500**;has_more |
| 语义搜索 | `POST https://pan.baidu.com/xpan/unisearch` | JSON;scene 固定 `mcpserver`;query 自然语言;OCR/ASR/以图搜图源 |
| 元数据 | `GET /xpan/file?method=meta`;`POST /xpan/file?method=filemetas`(路径数组);`GET /xpan/multimedia?method=filemetas`(**fsids ≤100**,取 dlink 用这个) | |
| 下载 | filemetas 取 dlink → `GET <dlink>&access_token=...` | **必须 Header `User-Agent: pan.baidu.com`**(否则 31326 防盗链);支持 Range;先 302;dlink 时效过期 31360 |
| 上传 | `POST /xpan/file?method=precreate`(content-md5/slice-md5/rtype)→ `POST <动态上传域名>/pcs/superfile2?method=upload`(partseq)→ `POST /xpan/file?method=create` | **上传域名须动态获取**;秒传=precreate 返回剩余 block_list 为空;单文件 4GB/会员 10GB/超会 20GB;分片 ≤1024;分片大小普通 4MB/会员 16MB/超会 32MB,**首片必须 4MB(31299)**;rtype 0失败/1重命名/2冲突重命名/3覆盖 |
| 单步上传 | `POST <上传域名>/pcs/file?method=upload`(ondup) | 仅小文件 |
| 配额 | `GET https://pan.baidu.com/api/quota` | total/used/free/expire |
| 管理 | `POST /xpan/file?method=filemanager`(opera=copy\|move\|rename\|delete;async;ondup);建目录 `method=create&isdir=1` | 异步任务 111 进行中 |
| 分享 | `POST https://pan.baidu.com/apaas/1.0/share/*` | **2026-09-16 起新体系,付费+企业开发者专属**;与旧分享不互通 |
| 回收站 | **官方未开放接口(文档站无)** | 未能核实≠存在 |

## 4. 限流与配额(预算器参数)

- 过审应用:**按需白名单分配,无公开数值表**;错误码 20012 超限 / 20013 无权限 / 20011 用户数超限 / 31034 频控
- 已知数值:未审核 10 次/小时;listall 建议 8-10 次/分钟
- **目录权限(2026-07 细则)**:应用文件操作默认**仅限 `/apps/{appname}`**(用户视角 `/我的应用数据/{appname}`);越界 20020 可被限流/收回——**扩权走白名单申请**
- 预算器建议初值(过审前):全局 8 req/min;listall 8/min;其余保守 30/min 起步,按 20012/31034 反馈自适应

## 5. 条款与合规要点

- 承诺函(2025-07-25):独立研发、不侵权、内容合法;平台可终止+索赔
- **使用规范原文**:禁止"利用个人网盘账号进行企业建站、图床、**搭建网盘迁移工具**等产品行为";禁止未授权下载/传播用户数据;禁止共享开发者账号 → **Partiverse 对百度盘的跨 Node 复制功能在 UI 层呈现为"整理/备份"语境,营销不提"迁移/搬运"(合规自查 §2 已登记法务确认项)**
- 品牌规范:产品界面须呈现百度网盘及开放平台标识(标识文件邮件索取);不得自称"百度网盘合作伙伴"
- 服务协议:平台可自行设置频次与用户数限制,不得规避

## 6. 已知坑与变更史(节选)

- 2026-09:分享旧接口下线;文档大改版(补充示例);fsids 批量元数据 + 付费"极速流量"(from_apaas)
- 2026-08:控制台"应用用途"双轨;百度网盘 MCP Server(SSE,file_list 等工具)
- 2026-07:目录权限细则;语义搜索 unisearch
- 2025-07:上线审核新流程生效
- 错误码速查:-6 鉴权失败 / -9 不存在 / -10 容量不足 / 20016-20017 token / 31034 频控 / 31299 首片 4MB / 31326 防盗链 / 31360 dlink 过期 / 31365 大小超限(含分片档位)

## 7. 未能核实(接入时补)

过审应用的具体 QPS/日配额数值;下载限速具体速率;独立"视频转码提交"接口;回收站接口;媒体点播"获取视频/音频信息"端点。
