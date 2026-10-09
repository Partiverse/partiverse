//! xtask:Partiverse 开发工具入口。
//!
//! 当前仅「版本命令」占位(打印自身版本,零依赖);门禁与构建编排由 M0/CI 任务扩展。

use std::process::ExitCode;

const USAGE: &str = "用法:cargo xtask version";

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        // 版本命令占位:打印自身版本。
        Some("version") => {
            println!("partiverse-xtask {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("未知命令:{other}\n{USAGE}");
            ExitCode::FAILURE
        }
        None => {
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}
