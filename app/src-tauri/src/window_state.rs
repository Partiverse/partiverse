//! Partiverse 文件级来源标注(整仓 AGPL-3.0,见根 LICENSE/NOTICE;本文件以 Apache-2.0 授权)。
//! 版权:Copyright PartiSync contributors,Apache License 2.0(<http://www.apache.org/licenses/LICENSE-2.0>)。
//! 来源:<https://github.com/Partiverse/partisync> `crates/partisync-desktop/src/window_state.rs`,
//! 基线 commit `d7f73ffd516ae455e9774f635332453bede6cfc1`(git show 只读取件;
//! ADR-0003 同款拷贝纪律,声明见 docs/adr/0008-wp05-frontend-dependencies.md)。
//! 本仓改动:仅追加本标注头,标注头之后正文与上游逐字节一致;唯一代码级适配 =
//! 测试模块以 std 临时目录等价物替换 `tempfile::TempDir`(免新增 Rust dev 依赖,
//! 三处测试断言逐条保留)。正文 `--data-dir` 接线表述属上游语境:本卡按
//! 「仅模块归属适配」未接线窗口事件,接线时须回改本注(M1-WP05-T01 报告知会项)。

//! 窗口位置/尺寸记忆（SPEC M6-WP03 §2.2 + §3 T06）。
//!
//! 主窗口关闭时把当前位置/尺寸持久化到 `<data-dir>/window-state.json`，
//! 下次启动恢复；首次启动（无状态文件）走 tauri.conf.json 的默认
//! 1200×800 + `center: true`。 全屏态**不**持久化（macOS 全屏关闭后
//! 重启恢复为非全屏， 满足 SPEC §3 T06 验收）。
//!
//! ## 设计
//! - 手写持久化而非引入 `tauri-plugin-window-state`： 免新顶层依赖
//!   （AGENTS.md 铁律 8 免 ADR 路径）， 且本 WP 只需「关闭时保存」
//!   一种时机， 无需插件的 per-event 保存。
//! - 坐标存**逻辑像素**（`to_logical(scale_factor)`）： 跨 DPI 显示器
//!   （retina → 外接 1×）恢复时几何关系正确。
//! - 全屏 / 退化尺寸（0×0， MockRuntime 或最小化）一律不保存 —— 捕获层
//!   返回 `None`， 调用方跳过写盘。
//! - 文件缺失 / JSON 损坏 → `load()` 返回 `None` → 回落默认几何，
//!   不 panic（对应 SPEC §2.6 错误语义的容错基调）。
//!
//! ## 测试
//! 纯函数层（[`capture_from`]）+ 文件层（[`WindowStateStore`]）可全单测；
//! 窗口层（[`capture`] / [`apply`]）在 `tests/commands.rs` 用
//! `tauri::test::MockRuntime` 冒烟（mock 的 setter 是 no-op、 getter 返回
//! 0×0 几何， 恰好覆盖「退化尺寸不保存」分支）， 真窗口行为归 T06 macOS
//! 手动验收。 SPEC §3 T06 验收即本模块行为契约（无 registry 属性条目，
//! 沿用 T03/T05 惯例引用 SPEC 节）。

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{
    LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize, Runtime, WebviewWindow, Window,
};

/// SPEC §2.2 默认窗口宽度（逻辑像素）。
pub const DEFAULT_WIDTH: f64 = 1200.0;
/// SPEC §2.2 默认窗口高度（逻辑像素）。
pub const DEFAULT_HEIGHT: f64 = 800.0;

/// 主窗口位置/尺寸（逻辑像素， 持久化于 `<data-dir>/window-state.json`）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// 持久化文件读写（路径由 `lib.rs` 按 `--data-dir` / 默认数据目录决定）。
#[derive(Debug, Clone)]
pub struct WindowStateStore {
    path: PathBuf,
}

impl WindowStateStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读取持久化状态。 文件缺失 / 不可读 / JSON 损坏 → `None`
    /// （调用方回落 tauri.conf.json 默认几何）。
    pub fn load(&self) -> Option<WindowState> {
        let raw = std::fs::read_to_string(&self.path).ok()?;
        serde_json::from_str(&raw).ok()
    }

    /// 写持久化状态（父目录不存在则创建）。
    ///
    /// # Errors
    /// 目录创建或文件写失败（`DataDirUnwritable` 场景）→ `io::Error`，
    /// 调用方（`on_window_event`）打 stderr 警告但不阻断关闭。
    pub fn save(&self, state: &WindowState) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(state)?;
        std::fs::write(&self.path, json)
    }
}

/// 纯函数： 物理几何 → 逻辑 [`WindowState`]。
///
/// 全屏或退化尺寸（0×0）→ `None`（不持久化）。 MockRuntime 的 getter
/// 恒返 0×0， 该守卫同时是测试环境的天然短路。
pub fn capture_from(
    scale_factor: f64,
    position: PhysicalPosition<i32>,
    inner: PhysicalSize<u32>,
    fullscreen: bool,
) -> Option<WindowState> {
    if fullscreen || inner.width == 0 || inner.height == 0 {
        return None;
    }
    let pos = position.to_logical::<f64>(scale_factor);
    let size = inner.to_logical::<f64>(scale_factor);
    Some(WindowState {
        x: pos.x,
        y: pos.y,
        width: size.width,
        height: size.height,
    })
}

/// 从活窗口捕获当前几何（逻辑像素）。 任一 getter 失败或命中守卫
/// （全屏 / 退化尺寸）→ `None`。 getter 失败按全屏处理（宁可不保存，
/// 不落盘垃圾数据）。
///
/// [`Window`] / [`WebviewWindow`] 各有一个薄包装（底层 getter 签名相同，
/// 无公共 trait）； `on_window_event` 回调给 `&Window`， setup/test 场景
/// 拿 `WebviewWindow`。
pub fn capture<R: Runtime>(window: &Window<R>) -> Option<WindowState> {
    capture_inner(window)
}

/// [`capture`] 的 [`WebviewWindow`] 变体（同守卫、同逻辑坐标契约）。
pub fn capture_webview<R: Runtime>(window: &WebviewWindow<R>) -> Option<WindowState> {
    capture_inner(window)
}

fn capture_inner<G: Geometry>(w: &G) -> Option<WindowState> {
    let fullscreen = w.geom_fullscreen();
    let scale_factor = w.geom_scale_factor();
    let position = w.geom_position()?;
    let inner = w.geom_inner_size()?;
    capture_from(scale_factor, position, inner, fullscreen)
}

/// 统一 [`Window`] / [`WebviewWindow`] 几何 getter 的小 trait（私有）。
trait Geometry {
    fn geom_fullscreen(&self) -> bool;
    fn geom_scale_factor(&self) -> f64;
    fn geom_position(&self) -> Option<PhysicalPosition<i32>>;
    fn geom_inner_size(&self) -> Option<PhysicalSize<u32>>;
}

macro_rules! impl_geometry {
    ($($t:ty),+ $(,)?) => {$(
        impl<R: Runtime> Geometry for $t {
            fn geom_fullscreen(&self) -> bool {
                // getter 失败按全屏处理： 宁可不保存， 不落盘垃圾数据。
                self.is_fullscreen().unwrap_or(true)
            }
            fn geom_scale_factor(&self) -> f64 {
                self.scale_factor().unwrap_or(1.0)
            }
            fn geom_position(&self) -> Option<PhysicalPosition<i32>> {
                self.outer_position().ok()
            }
            fn geom_inner_size(&self) -> Option<PhysicalSize<u32>> {
                self.inner_size().ok()
            }
        }
    )+};
}

impl_geometry!(Window<R>, WebviewWindow<R>);

/// 把持久化状态应用到窗口（启动恢复路径）。 `None` → 不动窗口，
/// 保持 tauri.conf.json 的默认几何（1200×800 + center）。
///
/// # Errors
/// `set_position` / `set_size` 失败 → `tauri::Error` 原样上抛
/// （setup 里转 `Box<dyn Error>`， 启动失败可见）。
pub fn apply<R: Runtime>(
    window: &WebviewWindow<R>,
    state: Option<WindowState>,
) -> tauri::Result<()> {
    let Some(state) = state else {
        return Ok(());
    };
    window.set_position(LogicalPosition::new(state.x, state.y))?;
    window.set_size(LogicalSize::new(state.width, state.height))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::PhysicalPosition;

    /// std 等价物:测试专用临时目录(进程 id+原子序号保证唯一,免 `tempfile`
    /// dev 依赖——搬运适配的唯一代码改动,断言逐条保留)。
    fn temp_dir(tag: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "partiverse-window-state-test-{}-{}-{tag}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&dir).expect("test temp dir must be creatable");
        dir
    }

    const STATE: WindowState = WindowState {
        x: 100.0,
        y: 50.0,
        width: DEFAULT_WIDTH,
        height: DEFAULT_HEIGHT,
    };

    #[test]
    fn serde_roundtrip_preserves_geometry() {
        let json = serde_json::to_string(&STATE).expect("serialize");
        let back: WindowState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, STATE);
    }

    #[test]
    fn save_then_load_roundtrip() {
        let tmp = temp_dir("roundtrip");
        let store = WindowStateStore::new(tmp.join("nested/window-state.json"));
        store.save(&STATE).expect("save");
        assert_eq!(store.load(), Some(STATE));
    }

    #[test]
    fn load_missing_file_returns_none() {
        let tmp = temp_dir("missing");
        let store = WindowStateStore::new(tmp.join("window-state.json"));
        assert_eq!(store.load(), None);
    }

    #[test]
    fn load_corrupt_json_returns_none() {
        let path = temp_dir("corrupt").join("window-state.json");
        std::fs::write(&path, "{not json").expect("write garbage");
        let store = WindowStateStore::new(path);
        assert_eq!(store.load(), None);
    }

    #[test]
    fn capture_from_fullscreen_returns_none() {
        let state = capture_from(
            1.0,
            PhysicalPosition::new(100, 50),
            PhysicalSize::new(1200, 800),
            true,
        );
        assert_eq!(state, None);
    }

    #[test]
    fn capture_from_zero_size_returns_none() {
        let state = capture_from(
            1.0,
            PhysicalPosition::new(0, 0),
            PhysicalSize::new(0, 0),
            false,
        );
        assert_eq!(state, None);
    }

    #[test]
    fn capture_from_hidpi_converts_to_logical() {
        // scale 2.0： 物理 (200, 100) 2400×1600 → 逻辑 (100, 50) 1200×800。
        let state = capture_from(
            2.0,
            PhysicalPosition::new(200, 100),
            PhysicalSize::new(2400, 1600),
            false,
        );
        assert_eq!(state, Some(STATE));
    }
}
