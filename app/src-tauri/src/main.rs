// 防止 Windows release 下出现额外控制台窗口,勿删!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 启动失败必须可见:stderr 记录后非零码退出(模板的 expect 违反生产路径红线,已移除)。
    if let Err(err) = app_lib::run() {
        eprintln!("partiverse app exited with error: {err}");
        std::process::exit(1);
    }
}
