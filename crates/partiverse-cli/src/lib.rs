//! partiverse-cli:Partiverse 命令行界面(M1-WP07-T01;Owner 2026-10-11 定向
//! 「先开放 CLI,逻辑清晰后 GUI 再跟上」)。
//!
//! 薄壳纪律:CLI 零业务逻辑,全部透传 crates(partiverse-core 表单/注入 +
//! partiverse-engine 安装器/协调器/rc 白名单客户端);手写子命令解析零 clap/tokio
//! (partisync-cli 形态先例,仅参考形态不拷代码)。**按次启动形态**:每个引擎类
//! 子命令在一次调用内 `ensure → 执行 → shutdown on exit`;`RcdLaunch` Drop 兜底
//! kill(引擎 CLI 退出 = 引擎退出 = 运行中 job 终止,任务卡显式声明)。
//! 错误经 `error_chain` 出 stderr + 退出码 0/1(engine-install bin 先例)。

pub mod browse;
pub mod connection;
pub mod engine;
pub mod session;
