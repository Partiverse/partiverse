# ADR-0005 WP02 rcd 监督器依赖集(nix;进程/信号类)

- 状态: 接受 | 日期: 2026-10-09 | 负责人: @partiverse
- 背景: M1-WP02-T02(rcd sidecar 监督器:spawn/探活/优雅退出/崩溃重启)需要向 rclone 子进程投递 SIGTERM 做优雅退出(2s 窗口,SIGKILL 兜底)。任务卡候选池:nix 或 libc 发 SIGTERM、tokio process,license 全 MIT/Apache,铁律 8 要求 ADR。仓库约束:deny.toml `multiple-versions = "deny"` 与 license 白名单禁改(ADR-0004 同款约束);作业红线禁止 unsafe。
- 决策: partiverse-engine 新增 1 个直接依赖 `nix 0.31.3`(`default-features = false`,`features = ["signal"]`,license=MIT,实测其 Cargo.toml;传递依赖 bitflags 2.13.2(MIT OR Apache-2.0)、cfg_aliases 0.2.2(MIT),均白名单内)。SIGTERM 投递用 `nix::sys::signal::kill` + `nix::unistd::Pid`(本卡实现处);SIGKILL 兜底与进程管理用 std(`std::process::Child::kill/wait`,std 在 unix 上 kill(2) 即 SIGKILL,无需第三方)。
  - **libc(候选池)否决**:`libc::kill` 为裸 FFI,调用需 `unsafe` 块,触作业红线(workspace 禁 unsafe,需先立 ADR 且无必要——nix 是其安全封装);nix 内部的 unsafe 属其自身审计范围,与工作区 crate 无涉。
  - **tokio process(候选池)否决**:T01 已裁定本 crate 公共 API 保持同步(ADR-0004),T02 沿用同步 API(spawn/探活/退避重启均为阻塞式,async 集成层放 `spawn_blocking` 即可),为此引入 tokio 运行时属以包装引重树;T03 如需异步编排再按程序立卡。
  - **随机性零依赖声明**(非依赖项,一并知会):卡内要求的加密随机源(随机 user/pass/socket 名)以 std 读取 `/dev/urandom` 实现(与 getrandom(2) 同源的内核 CSPRNG,`rcd.rs::random_bytes`);不引 rand/getrandom 直接依赖(树内 getrandom 0.2.17 仅为 ring 的传递依赖,未采用)。
- 理由: ①license 实测(读 registry 内 Cargo.toml,非记忆):nix=MIT、bitflags/cfg_aliases=MIT OR Apache-2.0,零商业授权,deny licenses 节白名单内;②`cargo tree --workspace --target all --duplicate` 实测零重复版本(nix 复用树内 libc 0.2.190,deny multiple-versions 节绿);③`default-features = false` + 仅 `signal` 特性收敛依赖面(nix 全特性树大得多);④被否选项再列:改用 `sh -c kill` 外部进程(引入 /bin 依赖与 argv 注入面,否决)、为 SIGTERM 引 tokio(见上,否决)、std-only 自旋等 SIGTERM 替代(std 无信号投递 API,不存在该路径)。
- 影响: 正向=SIGTERM 优雅退出(卡内 ③)以安全 API 落地,依赖树净增 3 包(1 直接 2 传递)且全白名单;代价=nix 0.31.3 为 2025 活跃维护版,未来 MSRV/平台表变更需跟随升级;非 unix 平台本模块显式 Fatal `RcdStartFailed` 上浮(卡内安全模式=unix socket,无 TCP 回退),nix 调用点均在 `#[cfg(unix)]` 门内,windows 目标编译不受影响。波及面=T03 编排层直接复用 `RcdSupervisor` 同步 API(spawn_blocking 包装)。
