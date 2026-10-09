//! CI/部署安装入口(M1-WP02-T01 卡 ⑧):按随仓 manifest 校验并安装 rclone 至默认
//! 引擎根(env `PARTIVERSE_ENGINE_ROOT` 可覆盖),供 T02/T03 测试使用。
//! 零 unwrap/expect;失败经 stderr 输出完整根因链并以退出码 1 结束。

use std::process::ExitCode;

use partiverse_engine::EngineInstaller;
use partiverse_engine::error::error_chain;

fn main() -> ExitCode {
    let installed = EngineInstaller::from_embedded_manifest().and_then(|installer| {
        EngineInstaller::resolve_install_root().and_then(|root| installer.ensure(&root))
    });
    match installed {
        Ok(path) => {
            println!("engine ready: {}", path.display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("engine install failed: {}", error_chain(&err));
            ExitCode::FAILURE
        }
    }
}
