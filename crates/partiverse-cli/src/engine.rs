//! engine 子命令(M1-WP07-T01 卡内 ② 第一优先面):install/ensure/probe/
//! status/shutdown,全部透传 crates(`EngineInstaller`/`EngineCoordinator`),
//! CLI 零业务逻辑。install 复用 engine-install bin 语义;ensure/probe/shutdown
//! 走按次启动会话(见 [`crate::session`]);status 只读盘面不拉引擎。

use partiverse_engine::{EngineError, EngineInstaller, EngineManifest};

use crate::session::Session;

/// `engine install`:幂等安装引擎至安装根(env `PARTIVERSE_ENGINE_ROOT` 可
/// 覆盖),完全复用 engine-install bin 语义(快路径 sha256 复验 / 缺失下载)。
///
/// # Errors
/// 安装器/安装根解析失败原样上浮(`EngineError`,stderr 经 error_chain 出根因链)。
pub fn install() -> Result<String, EngineError> {
    let installer = EngineInstaller::from_embedded_manifest()?;
    let root = EngineInstaller::resolve_install_root()?;
    let path = installer.ensure(&root)?;
    Ok(format!("engine ready: {}", path.display()))
}

/// `engine status`:只读盘面报告(安装根/钉定版本/二进制就绪与否),不拉引擎。
/// 未安装非错误(状态查询语义),报告文本如实呈现。
///
/// # Errors
/// 内嵌 manifest 解析或安装根解析失败原样上浮(零吞错)。
pub fn status() -> Result<String, EngineError> {
    let manifest = EngineManifest::embedded()?;
    let root = EngineInstaller::resolve_install_root()?;
    let asset = manifest.current_platform()?;
    let binary = root.join(&manifest.version).join(&asset.binary_name);
    let state = if binary.is_file() {
        "installed"
    } else {
        "not installed"
    };
    Ok(format!(
        "engine version {} ({state})\ninstall root: {}\nbinary: {}",
        manifest.version,
        root.display(),
        binary.display()
    ))
}

/// `engine ensure`:按次启动会话全链(ensure → 句柄观测面输出 → 优雅退出)。
/// CLI 退出 = 引擎退出(RcdLaunch Drop 兜底,任务卡显式声明)。
///
/// # Errors
/// ensure/shutdown 失败原样上浮(零吞错)。
pub fn ensure() -> Result<String, EngineError> {
    let mut session = Session::open()?;
    let handle = session.handle();
    let report = format!(
        "engine ready: slot `{}` pid {:?} socket {}",
        handle.slot_id,
        handle.pid,
        handle.socket_path.display()
    );
    session.shutdown()?;
    Ok(report)
}

/// `engine probe`:ensure → `core/version` 探活输出版本 → 优雅退出。
///
/// # Errors
/// 探活/shutdown 失败原样上浮(零吞错)。
pub fn probe() -> Result<String, EngineError> {
    let mut session = Session::open()?;
    let version = session.probe()?;
    session.shutdown()?;
    Ok(format!("engine core version: {version}"))
}

/// `engine shutdown`:ensure → 立即优雅退出(SIGTERM + socket 清理断言),
/// 验证优雅退出全链;按次启动形态下持久引擎不存在,本命令即生命周期自检面。
///
/// # Errors
/// ensure/shutdown 失败原样上浮(零吞错)。
pub fn shutdown() -> Result<String, EngineError> {
    let mut session = Session::open()?;
    session.shutdown()?;
    Ok("engine shut down cleanly".into())
}
