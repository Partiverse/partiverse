# pv 真机体验手册(Owner 版)

> 目标:15 分钟走完「装引擎 → 建连接 → 浏览 → 建目录 → 复制 → 看队列」全链。
> 每一步都写了**预期输出**;任何一步与预期不符,跳到 §5 排障(把命令和日志发给 AI 即可定位)。

## 0. 准备(只做一次)

### 0.1 让 `pv` 命令可用

二选一:

```bash
# 方式 A:每次走 cargo run(免安装,略慢 ~0.5s)
echo "alias pv='cargo run -q --release -p partiverse-cli --'" >> ~/.zshrc && source ~/.zshrc

# 方式 B:编译一次,直接用二进制(推荐)
cargo build --release -p partiverse-cli
echo "alias pv='/home/acme/Codebase/partiverse/target/release/pv'" >> ~/.zshrc && source ~/.zshrc
```

验证:`pv engine status` 有输出即成功。

### 0.2 需要提前知道的「三条稳定性铁则」

1. **每条 `pv` 命令 = 启动引擎 → 执行 → 关引擎**。所以传输命令**必须加 `--wait`**,否则命令一结束,后台任务跟着引擎一起被杀(任务会标成 error)。
2. 引擎中途被杀(手滑 Ctrl-C / 进程崩溃)后,跑一次 `pv jobs reconcile` 对账——它会帮你把孤儿任务标记清楚。
3. **`pv rm` 是递归删除**(目标树下所有文件都会删),命令会要求你输入 `yes`;输错了或直接回车 = 什么都不做(安全)。

---

## 1. 引擎:安装与启动

```bash
pv engine install     # 第一次:按锁定版本下载 rclone 并校验(本机已预装则秒过)
pv engine ensure      # 启动引擎并探活
pv engine probe       # 查看引擎版本,预期输出含 "1.75.1"
pv engine status      # 查看引擎状态
```

预期:`ensure` 输出引擎就绪信息;`probe` 输出 `1.75.1`。
不符?→ §5 排障。

## 2. 建连接(配置持久化,建一次一直用)

```bash
# 本地目录(体验用,零外部依赖)
pv connection create-local mydisk --path /home/acme/Documents

# WebDAV(如 123 云盘;端点从 123 网页端「第三方挂载→WebDAV 授权管理」复制)
pv connection create-webdav my123 --url https://webdav-xxxx.123pan.cn/webdav --user <你的手机号/邮箱> --pass <应用专用密码>

pv connection list    # 查看已建连接
```

预期:`connection created: <name>`;`connection list` 能看到 mydisk(和 my123)。
注意:`--pass` 只进配置文件(密码混淆),**不会出现在屏幕、日志或错误信息里**。

## 3. 浏览

```bash
pv ls mydisk:                  # 列根目录
pv ls mydisk:/子目录           # 列子目录
```

预期:表格输出(名称/类型/大小/修改时间),目录排前面。
不符(比如空白/报错)→ §5,把命令+输出发给 AI。

## 4. 文件操作与传输

```bash
pv mkdir mydisk:/pv-test                    # 新建目录
pv copy mydisk:/pv-test my123:/Backup --wait  # 跨盘复制(--wait=等它跑完)
pv move mydisk:/pv-test mydisk:/pv-done --wait
pv rm mydisk:/pv-test                       # 递归删除,要求输入 yes
```

预期:`copy/move --wait` 结束时显示任务终态(Done);中途想看进度,Ctrl-C 不会杀传输(引擎独立于命令?**不——见铁则 1,传输期间别 Ctrl-C**)。
更稳的观察方式:另开一个终端跑 `pv jobs list --watch`(实时刷新,直到队列清空)。

## 5. 任务队列

```bash
pv jobs list               # 全部任务
pv jobs list --watch       # 实时观察,直到队列清空(Ctrl-C 可退)
pv jobs poll <任务id>       # 查单个任务详情
pv jobs cancel <任务id>     # 取消运行中任务
pv jobs retry <任务id>      # 重试失败任务
pv jobs reconcile          # 引擎重启后对账(铁则 2 的补救动作)
```

预期:`list` 表格含状态(Done/Error/Running/Queued)与错误摘要。

## 6. 日志:你的每一步都有记录

每次 `pv` 命令自动追加一条日志到:

```bash
~/.local/state/partiverse/logs/pv.log
```

```bash
tail -20 ~/.local/state/partiverse/logs/pv.log     # 看最近 20 条
grep ERROR ~/.local/state/partiverse/logs/pv.log   # 只看错误
tail -f ~/.local/state/partiverse/logs/pv.log      # 实时滚动(另开终端)
```

日志里每条记录 = 时间 + 你敲的命令 + 退出码 + 耗时(+错误时的完整错误链)。
`--pass` 等敏感参数自动打码为 `***`,放心跑。
日志超过 5MB 自动轮转(pv.log → pv.log.1),不用管大小。
临时不想记录:`PARTIVERSE_LOG=off pv ls mydisk:`。

## 7. 出问题了怎么反馈

把下面两样东西发给 AI(直接粘贴即可):

```bash
# 1. 你跑的命令与输出
<paste 在这里>

# 2. 日志尾部
tail -30 ~/.local/state/partiverse/logs/pv.log
```

## 8. 常见错误速查

| 现象 | 大概率原因 | 处置 |
|---|---|---|
| `engine` 类命令报错说找不到引擎 | 引擎未安装到默认位置 | `pv engine install`;或本机已预装→确认 `~/.local/share/partiverse/engine/1.75.1/rclone` 存在 |
| 传输后 `jobs list` 显示 Error(engine_restarted) | 命令结束时引擎退出,任务被对账标记 | 属预期;`pv jobs retry <id>` 重跑 |
| ls 空白但连接存在 | 远端目录真为空 | 换 `pv ls my123:/` 根路径再试 |
| WebDAV 连接后操作报 401/超时 | 123 端点/应用专用密码问题(非 pv 问题) | 核对端点从控制台复制、密码是「应用专用密码」非登录密码 |
| 中文文件名列显示宽度怪 | 已按 CJK 双宽对齐,若仍异常 | 截图发 AI(已修一轮,可能还有边角) |

---

*功能覆盖说明:本手册覆盖 M1 已交付的 CLI 面(引擎/连接/浏览/文件操作/传输/队列/日志)。尚未进 CLI 的:预览(serve http)、哈希校验、加密配置(`config encrypt`)、百度直链(等 backends-go)。*
