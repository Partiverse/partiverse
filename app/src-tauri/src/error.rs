//! IPC 命令错误形状:统一 `{ kind, msg, severity }` 线格式。
//!
//! 形状照 partisync 桌面壳 `desktop error.rs` 的 `{kind,msg}` 模式按本仓语境重写
//! (docs/tasks/M1-WP01-T02.md 边界条款;非逐字节拷贝,不携带上游许可头)。
//! 约定:`kind` = 机器可判定的错误类别(小写蛇形),`msg` = 给人看的消息,
//! `severity` = crate 三级严重度镜像;错误必须经本类型上浮 UI 或日志(禁止吞错)。
//! M1-WP05-T01:新增 `Engine`/`Core` 类别与 `severity` 字段(卡内 DoD① 明示授权)。

use partiverse_core::error::PartisyError;
use partiverse_engine::error::{error_chain, EngineError};
use serde::Serialize;
use specta::Type;
use std::error::Error as _;

/// 错误严重度三级镜像(线值与 core `Severity` 的 Display 一致)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// 瞬态:重试可能成功。
    Retryable,
    /// 持久:重试无意义。
    Fatal,
    /// 应用层显式中断。
    Interrupted,
}

impl From<partiverse_core::error::Severity> for Severity {
    fn from(value: partiverse_core::error::Severity) -> Self {
        match value {
            partiverse_core::error::Severity::Retryable => Self::Retryable,
            partiverse_core::error::Severity::Fatal => Self::Fatal,
            partiverse_core::error::Severity::Interrupted => Self::Interrupted,
        }
    }
}

/// 错误类别。IPC 线上序列化为字符串(snake_case);新增类别须回写任务卡边界说明
/// (M1-WP05-T01 新增 Engine/Core 已回写运行报告)。
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
    Internal,
    /// 引擎面错误(partiverse-engine:M1-WP05-T01 新增,severity 保真)。
    Engine,
    /// 核心面错误(partiverse-core:M1-WP05-T01 新增,severity 保真)。
    Core,
}

/// 命令错误顶层类型;经 serde 序列化为 `{ kind, msg, severity }`,由 specta 生成对应 TS 类型。
#[derive(Debug, Clone, Serialize, Type)]
pub struct CmdError {
    pub kind: ErrorKind,
    pub msg: String,
    pub severity: Severity,
}

impl CmdError {
    #[must_use]
    pub fn new(kind: ErrorKind, severity: Severity, msg: impl Into<String>) -> Self {
        Self {
            kind,
            msg: msg.into(),
            severity,
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
        Self::new(
            ErrorKind::Io,
            partiverse_core::error::classify_io(err.kind()).into(),
            err.to_string(),
        )
    }
}

impl From<EngineError> for CmdError {
    fn from(err: EngineError) -> Self {
        Self::new(ErrorKind::Engine, err.severity().into(), error_chain(&err))
    }
}

impl From<PartisyError> for CmdError {
    fn from(err: PartisyError) -> Self {
        // Display 已含「severity: 一级来源」,链从 source 起拼避免一级重复。
        let msg = match err.source() {
            Some(source) => error_chain(source),
            None => err.to_string(),
        };
        Self::new(ErrorKind::Core, err.severity.into(), msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use partiverse_core::error::Severity as CoreSeverity;

    /// IPC 线格式契约:`{ "kind": "io", "msg": "...", "severity": "fatal" }`
    /// (snake_case 类别与严重度)。
    #[test]
    fn serializes_to_kind_msg_severity_shape() {
        let err = CmdError::new(ErrorKind::Io, Severity::Fatal, "disk full");
        let json = serde_json::to_value(&err).expect("test serialization must not fail");
        assert_eq!(json["kind"], "io");
        assert_eq!(json["msg"], "disk full");
        assert_eq!(json["severity"], "fatal");
    }

    /// IPC 线格式契约:全部 kind/severity 的 snake_case 线值
    /// (前端按此字符串分支,变更即破坏性)。
    #[test]
    fn all_error_kinds_and_severities_wire_values_are_snake_case() {
        for (kind, wire) in [
            (ErrorKind::Io, "io"),
            (ErrorKind::Config, "config"),
            (ErrorKind::Internal, "internal"),
            (ErrorKind::Engine, "engine"),
            (ErrorKind::Core, "core"),
        ] {
            let json = serde_json::to_value(CmdError::new(kind, Severity::Fatal, "x"))
                .expect("test serialization must not fail");
            assert_eq!(json["kind"], wire);
            assert_eq!(json["msg"], "x");
            assert_eq!(json["severity"], "fatal");
        }
        for (severity, wire) in [
            (Severity::Retryable, "retryable"),
            (Severity::Fatal, "fatal"),
            (Severity::Interrupted, "interrupted"),
        ] {
            let json = serde_json::to_value(CmdError::new(ErrorKind::Io, severity, "x"))
                .expect("test serialization must not fail");
            assert_eq!(json["severity"], wire);
        }
    }

    /// crate 错误 → CmdError 映射:severity 保真、kind 归面、消息链保留。
    #[test]
    fn crate_errors_map_with_severity_fidelity() {
        let core_err = PartisyError::with_source(
            CoreSeverity::Retryable,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "socket gone",
            )),
        );
        let mapped: CmdError = core_err.into();
        assert_eq!(mapped.kind, ErrorKind::Core);
        assert_eq!(mapped.severity, Severity::Retryable);
        assert!(mapped.msg.contains("socket gone"), "chain lost: {mapped}");

        let engine_err = EngineError::from_io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "denied",
        ));
        let mapped: CmdError = engine_err.into();
        assert_eq!(mapped.kind, ErrorKind::Engine);
        assert_eq!(mapped.severity, Severity::Fatal);
        assert!(mapped.msg.contains("denied"), "chain lost: {mapped}");
    }
}
