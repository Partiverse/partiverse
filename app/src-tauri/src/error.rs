//! IPC 命令错误形状:统一 `{ kind, msg }` 线格式。
//!
//! 形状照 partisync 桌面壳 `desktop error.rs` 的 `{kind,msg}` 模式按本仓语境重写
//! (docs/tasks/M1-WP01-T02.md 边界条款;非逐字节拷贝,不携带上游许可头)。
//! 约定:`kind` = 机器可判定的错误类别(小写蛇形),`msg` = 给人看的消息;
//! 错误必须经本类型上浮 UI 或日志(禁止吞错/静默降级)。

use serde::Serialize;
use specta::Type;

/// 错误类别。IPC 线上序列化为字符串(snake_case);新增类别须回写任务卡边界说明。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// 本地 IO(配置、缓存等读写失败)。
    Io,
    /// 配置缺失或非法。
    ///
    /// IPC 线格式契约类别:生产代码构造点随首个配置读取命令落地。非测试构建下以
    /// `expect` 声明「尚未构造」——一旦出现构造点会因期望未失效报警,届时删除本属性
    /// (优于 allow 永久压制);测试 target 构造它以钉住线格式值,故属性限定 not(test)。
    #[cfg_attr(not(test), expect(dead_code))]
    Config,
    /// 未归类内部错误(兜底;分类困难时不得用它掩盖可判定类别)。
    ///
    /// IPC 线格式契约类别:生产代码构造点随首个运行时兜底转换(如 From<tauri::Error>)
    /// 落地,届时删除本属性(机制同上)。
    #[cfg_attr(not(test), expect(dead_code))]
    Internal,
}

/// 命令错误顶层类型;经 serde 序列化为 `{ kind, msg }`,由 specta 生成对应 TS 类型。
#[derive(Debug, Clone, Serialize, Type)]
pub struct CmdError {
    pub kind: ErrorKind,
    pub msg: String,
}

impl CmdError {
    #[must_use]
    pub fn new(kind: ErrorKind, msg: impl Into<String>) -> Self {
        Self {
            kind,
            msg: msg.into(),
        }
    }
}

impl std::fmt::Display for CmdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.msg)
    }
}

impl std::error::Error for CmdError {}

impl From<std::io::Error> for CmdError {
    fn from(err: std::io::Error) -> Self {
        Self::new(ErrorKind::Io, err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// IPC 线格式契约:`{ "kind": "io", "msg": "..." }`(snake_case 类别)。
    #[test]
    fn serializes_to_kind_msg_shape() {
        let err = CmdError::new(ErrorKind::Io, "disk full");
        let json = serde_json::to_value(&err).expect("test serialization must not fail");
        assert_eq!(json["kind"], "io");
        assert_eq!(json["msg"], "disk full");
    }

    /// IPC 线格式契约:全部 kind 的 snake_case 线值(前端按此字符串分支,变更即破坏性)。
    #[test]
    fn all_error_kinds_wire_values_are_snake_case() {
        for (kind, wire) in [
            (ErrorKind::Io, "io"),
            (ErrorKind::Config, "config"),
            (ErrorKind::Internal, "internal"),
        ] {
            let json = serde_json::to_value(CmdError::new(kind, "x"))
                .expect("test serialization must not fail");
            assert_eq!(json["kind"], wire);
            assert_eq!(json["msg"], "x");
        }
    }
}
