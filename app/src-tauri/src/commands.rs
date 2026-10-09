//! IPC 命令面(M1-WP01-T02:仅示范命令,不含任何业务功能)。
//!
//! 业务命令后续按 08 规划 §10.4 的命名空间(browse/transfer/search/profile)扩展,
//! 本卡只交付 `app_version` 用于打通 specta → TS 类型管道。

use crate::error::CmdError;
// `package_info` 在 Tauri 2 里是 AppHandle 的固有方法,无需 Manager trait(cargo check 实证)。
use tauri::{AppHandle, Wry};

/// 示范命令:返回应用版本号(验证 specta 强类型 IPC 管道与 `{kind,msg}` 错误通道)。
///
/// 返回 `Result` 是刻意的:让生成的 TS 绑定同时覆盖 ok/error 两个形状
/// (tauri-specta 默认 `ErrorHandlingMode::Result`)。
#[tauri::command]
#[specta::specta]
pub fn app_version(app: AppHandle<Wry>) -> Result<String, CmdError> {
    Ok(app.package_info().version.to_string())
}
