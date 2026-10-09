//! Partiverse 桌面壳(Tauri 2 应用壳,M1-WP01-T02)。
//!
//! 职责边界:壳层装配 + specta IPC 类型管道;sidecar 引擎管理属 WP02,
//! 业务功能一律不在本卡范围(docs/tasks/M1-WP01-T02.md)。

mod commands;
mod error;

/// 组装 IPC 命令面:注册命令并产出可导出的 specta Builder。
/// 启动路径与绑定导出测试共用同一构造,保证「命令 ↔ 生成绑定」单一事实源。
fn ipc_builder() -> tauri_specta::Builder<tauri::Wry> {
    tauri_specta::Builder::<tauri::Wry>::new()
        .commands(tauri_specta::collect_commands![commands::app_version,])
}

/// 导出 TS IPC 绑定到前端 `src/bindings.ts`(生成物,不入库)。
///
/// 路径钉在 `CARGO_MANIFEST_DIR`,不随进程 CWD 漂移;仅 debug 构建执行,
/// `cargo test` 走同一函数——系统 GUI 依赖就绪前也能离线生成绑定。
#[cfg(debug_assertions)]
fn export_bindings(
    builder: &tauri_specta::Builder<tauri::Wry>,
) -> Result<(), Box<dyn std::error::Error>> {
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/bindings.ts");
    builder.export(specta_typescript::Typescript::default(), out)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let builder = ipc_builder();

    // 仅 debug 导出绑定(官方推荐模式);失败必须上浮,禁止静默跳过。
    #[cfg(debug_assertions)]
    export_bindings(&builder)?;

    tauri::Builder::default()
        .invoke_handler(builder.invoke_handler())
        .setup(move |app| {
            builder.mount_events(app);
            Ok(())
        })
        .run(tauri::generate_context!())?;
    Ok(())
}

#[cfg(all(test, debug_assertions))]
mod tests {
    use super::*;

    /// specta 管道测试:不启动 GUI 即验证「命令 → TS 绑定」导出可用。
    /// 产物即前端消费的 `src/bindings.ts`(生成物)。
    #[test]
    fn export_bindings_writes_frontend_bindings() {
        let result = export_bindings(&ipc_builder());
        assert!(result.is_ok(), "specta 绑定导出失败: {result:?}");
    }
}
