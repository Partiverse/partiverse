//! 引擎分发器错误体系(M1-WP02-T01)。
//!
//! 约束:每个错误携带 partiverse-core [`Severity`] 上浮,零吞错、零静默降级;
//! 只用 Fatal/Retryable(Interrupted 按核心约定仅应用层可构造);`kind()` 结构化
//! 供单测直接断言,不依赖字符串匹配。

use std::fmt;

use partiverse_core::error::{Severity, classify_io};

/// 结构化错误类别(上层决策与测试断言面)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineErrorKind {
    /// manifest 读取/解析/字段校验失败(数据格式,Fatal);载荷=失败点描述。
    ManifestInvalid(String),
    /// 宿主平台不在 manifest 平台表(Fatal);载荷=平台键。
    PlatformUnsupported(String),
    /// 安装根无法解析(Fatal);载荷=失败点描述。
    InstallRootUnresolved(String),
    /// 文件系统 IO 错误(severity 由核心 `classify_io` 表驱动)。
    Io,
    /// 有序多源全部失败(Retryable);载荷=每源一条尝试记录,零吞错。
    DownloadFailed(Vec<SourceAttempt>),
    /// 手动导入 zip sha256 与 manifest 不符(同一文件重试无意义,Fatal)。
    ImportHashMismatch {
        /// manifest 锚定摘要(小写 hex)。
        expected: String,
        /// 实测摘要(小写 hex)。
        actual: String,
    },
    /// zip 损坏或引擎二进制条目缺失(Fatal);载荷=失败点描述。
    ExtractFailed(String),
    /// 加密随机源不可用(rcd 随机凭据/socket 名生成失败,凭据缺失不可降级,Fatal)。
    RandomSourceUnavailable(String),
    /// rcd 进程无法启动(二进制缺失/不可执行,重试无意义,Fatal);载荷=失败点描述。
    RcdStartFailed(String),
    /// rcd 就绪探活在窗口内未通过(冷启动慢/瞬态不可达,Retryable);载荷=窗口与最后探活根因。
    RcdNotReady(String),
    /// rcd 上报版本与 manifest 锁定版本不符(装错引擎,重装才有意义,Fatal)。
    RcdVersionMismatch {
        /// manifest 锚定期望版本(如 "v1.75.1")。
        expected: String,
        /// rcd 实际上报版本。
        actual: String,
    },
    /// 优雅退出未完成(SIGTERM 投递异常/收尾清理失败,Fatal);载荷=失败点描述。
    RcdShutdownFailed(String),
    /// 崩溃重启耗尽指数退避(≤5 次)进入 Failed 终态(Fatal)。
    RcdRestartExhausted {
        /// 本幕已尝试的重启次数。
        attempts: u32,
        /// 最后一次失败的根因(零静默)。
        last_cause: String,
    },
    /// 监督器状态机非法调用(如 Exited 后再 ensure_running,编程错误,Fatal);载荷=状态与调用。
    RcdInvalidState(String),
}

/// 单个下载源的一次尝试记录(失败原因全量上浮)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAttempt {
    /// 源名(manifest.sources[].name)。
    pub source: String,
    /// 本次尝试的完整下载 URL。
    pub url: String,
    /// 失败原因。
    pub cause: SourceFailureKind,
}

/// 单源失败原因(区分传输类与内容类,供上层区分「换源/换网」)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceFailureKind {
    /// 传输层失败(DNS/连接/超时/响应体中断/非 HTTPS 被拒等)。
    Transport(String),
    /// 服务器返回非 2xx 状态。
    HttpStatus(u16),
    /// 下载内容 sha256 与 manifest 不符(换源可能取到正确字节)。
    HashMismatch {
        /// manifest 锚定摘要(小写 hex)。
        expected: String,
        /// 实测摘要(小写 hex)。
        actual: String,
    },
    /// 内容通过 sha256 但解包失败(该源内容损坏,换源重试)。
    ExtractFailed(String),
}

/// 引擎分发器顶层错误:severity 机器可判定,kind 结构化,source 为根因链。
#[derive(Debug)]
pub struct EngineError {
    severity: Severity,
    kind: EngineErrorKind,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl EngineError {
    /// 构造错误(severity 由调用方按类别文档语义给定)。
    #[must_use]
    pub fn new(kind: EngineErrorKind, severity: Severity) -> Self {
        EngineError {
            severity,
            kind,
            source: None,
        }
    }

    /// 附加根因(`source()` 供上层拼接完整因果链)。
    #[must_use]
    pub fn with_source(mut self, source: Box<dyn std::error::Error + Send + Sync>) -> Self {
        self.source = Some(source);
        self
    }

    /// IO 错误构造:severity 由 partiverse-core `classify_io` 表驱动,与核心语义一致。
    #[must_use]
    pub fn from_io(err: std::io::Error) -> Self {
        EngineError {
            severity: classify_io(err.kind()),
            kind: EngineErrorKind::Io,
            source: Some(Box::new(err)),
        }
    }

    /// 错误严重度(作业系统重试/放弃决策的唯一依据)。
    #[must_use]
    pub fn severity(&self) -> Severity {
        self.severity
    }

    /// 结构化错误类别。
    #[must_use]
    pub fn kind(&self) -> &EngineErrorKind {
        &self.kind
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "engine installer ({}): {}", self.severity, self.kind)
    }
}

impl fmt::Display for EngineErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineErrorKind::ManifestInvalid(detail) => write!(f, "manifest invalid: {detail}"),
            EngineErrorKind::PlatformUnsupported(platform) => {
                write!(f, "platform unsupported: {platform}")
            }
            EngineErrorKind::InstallRootUnresolved(detail) => {
                write!(f, "install root unresolved: {detail}")
            }
            EngineErrorKind::Io => write!(f, "io error"),
            EngineErrorKind::DownloadFailed(attempts) => {
                write!(f, "all {} source(s) failed:", attempts.len())?;
                for attempt in attempts {
                    write!(f, " [{}] {}", attempt.source, attempt.cause)?;
                }
                Ok(())
            }
            EngineErrorKind::ImportHashMismatch { expected, actual } => {
                write!(
                    f,
                    "import hash mismatch: expected {expected}, actual {actual}"
                )
            }
            EngineErrorKind::ExtractFailed(detail) => write!(f, "extract failed: {detail}"),
            EngineErrorKind::RandomSourceUnavailable(detail) => {
                write!(f, "random source unavailable: {detail}")
            }
            EngineErrorKind::RcdStartFailed(detail) => write!(f, "rcd start failed: {detail}"),
            EngineErrorKind::RcdNotReady(detail) => write!(f, "rcd not ready: {detail}"),
            EngineErrorKind::RcdVersionMismatch { expected, actual } => {
                write!(
                    f,
                    "rcd version mismatch: expected {expected}, actual {actual}"
                )
            }
            EngineErrorKind::RcdShutdownFailed(detail) => {
                write!(f, "rcd shutdown failed: {detail}")
            }
            EngineErrorKind::RcdRestartExhausted {
                attempts,
                last_cause,
            } => {
                write!(
                    f,
                    "rcd restart exhausted after {attempts} attempt(s): {last_cause}"
                )
            }
            EngineErrorKind::RcdInvalidState(detail) => write!(f, "rcd invalid state: {detail}"),
        }
    }
}

impl fmt::Display for SourceFailureKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceFailureKind::Transport(detail) => write!(f, "transport: {detail}"),
            SourceFailureKind::HttpStatus(code) => write!(f, "http status {code}"),
            SourceFailureKind::HashMismatch { expected, actual } => {
                write!(f, "hash mismatch: expected {expected}, actual {actual}")
            }
            SourceFailureKind::ExtractFailed(detail) => write!(f, "extract failed: {detail}"),
        }
    }
}

impl std::error::Error for EngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|b| &**b as _)
    }
}

/// 拼接错误根因链(顶层 Display + `source()` 逐级;诊断输出零吞错)。
/// bin 目标是独立 crate,故必须 pub 而非 pub(crate)。
pub fn error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut text = err.to_string();
    let mut current = err.source();
    while let Some(cause) = current {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        current = cause.source();
    }
    text
}
