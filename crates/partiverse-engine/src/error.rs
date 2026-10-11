//! 引擎分发器错误体系(M1-WP02-T01)。
//!
//! 约束:每个错误携带 partiverse-core [`Severity`] 上浮,零吞错、零静默降级;
//! 只用 Fatal/Retryable(Interrupted 按核心约定仅应用层可构造);`kind()` 结构化
//! 供单测直接断言,不依赖字符串匹配。

use std::fmt;
use std::time::Duration;

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
        /// manifest 锁定期望版本(如 "v1.75.1")。
        expected: String,
        /// rcd 实际上报版本。
        actual: String,
    },
    /// 加密 config 无法以注入密钥解密/校验异常(凭据失效或装错密钥,重试
    /// 无意义,Fatal);载荷=诊断文本(零密钥)。(M1-WP04-T01 接缝新增)
    RcdConfigDecryptFailed(String),
    /// 加密 config 注入要素解析失败(M1-WP99-T03 接缝新增):默认 config 路径
    /// 解析(`rclone config file`)非零退出/载荷不可解析,或 core 注入决策失败
    /// (含加密 config 缺主密钥文件的 Fail-closed Fatal);载荷=根因摘要,
    /// `source()` = 原始错误链(零吞错)。
    ConfigInjectionUnresolved(String),
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
    /// 槽位配置非法(解析/字段校验失败/显式 env 文件缺失,Fatal);载荷=失败点描述。
    SlotConfigInvalid(String),
    /// 请求的槽位 id 不在已加载槽位表中(配置数据问题,Fatal);载荷=slot id。
    SlotNotFound(String),
    /// 引擎协调器状态机非法调用(单实例槽位冲突/未知槽位 shutdown/probe,
    /// 编程或配置错误,Fatal);载荷=描述。
    CoordinatorInvalidState(String),
    // —— 以下为 M1-WP03-T01 rc 客户端新增(载荷字段:method=canonical 名、
    // status=HTTP 状态、detail=rc error 文本或原始体截断)——
    /// rc 白名单外方法本地拒绝(编程或注入错误,请求未出本进程,Fatal);
    /// 载荷=被拒方法名(错误信息含名,卡内 ②)。
    RcMethodRejected(String),
    /// rc 请求参数序列化失败(Fatal);载荷=失败点描述。
    RcRequestInvalid(String),
    /// rc 应答协议不合法(重试同引擎无意义,Fatal);截断/中断瞬态走 `Io`。
    RcResponseInvalid(String),
    /// rc 单次调用超时(默认 10s 可配置,防僵死;Retryable);载荷=配置值。
    RcTimeout { timeout: Duration },
    /// rc 限流(Throttled 标记 = 交 T03 配额预算器退避消费,不直接抛 UI,
    /// 架构 §3;Retryable)。
    RcThrottled {
        method: String,
        status: u16,
        detail: String,
    },
    /// rc 服务端错误(HTTP 5xx,Retryable);载荷=命令/状态/详情。
    RcServerError {
        method: String,
        status: u16,
        detail: String,
    },
    /// rc 客户端类错误(HTTP 4xx 参数/权限等,非 429/503,重试无意义,Fatal);
    /// 载荷=命令/状态/详情。
    RcClientError {
        method: String,
        status: u16,
        detail: String,
    },
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
            EngineErrorKind::RcdConfigDecryptFailed(detail) => {
                write!(f, "rcd config decrypt failed: {detail}")
            }
            EngineErrorKind::ConfigInjectionUnresolved(detail) => {
                write!(f, "config injection unresolved: {detail}")
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
            EngineErrorKind::SlotConfigInvalid(detail) => {
                write!(f, "slot config invalid: {detail}")
            }
            EngineErrorKind::SlotNotFound(slot_id) => write!(f, "slot not found: {slot_id}"),
            EngineErrorKind::CoordinatorInvalidState(detail) => {
                write!(f, "engine coordinator invalid state: {detail}")
            }
            EngineErrorKind::RcMethodRejected(m) => write!(f, "rc method not whitelisted: {m}"),
            EngineErrorKind::RcRequestInvalid(d) => write!(f, "rc request invalid: {d}"),
            EngineErrorKind::RcResponseInvalid(d) => write!(f, "rc response invalid: {d}"),
            EngineErrorKind::RcTimeout { timeout } => {
                write!(f, "rc call timed out after {timeout:?}")
            }
            EngineErrorKind::RcThrottled {
                method,
                status,
                detail,
            } => {
                write!(f, "rc throttled ({method}, http {status}): {detail}")
            }
            EngineErrorKind::RcServerError {
                method,
                status,
                detail,
            } => {
                write!(f, "rc server error ({method}, http {status}): {detail}")
            }
            EngineErrorKind::RcClientError {
                method,
                status,
                detail,
            } => {
                write!(f, "rc client error ({method}, http {status}): {detail}")
            }
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
///
/// 相邻重复段折叠(M1-WP07-T05 D2,dogfood 实录):顶层 Display 常已内嵌
/// 完整根因文本(如 `retryable: engine installer (retryable): rc server
/// error (...): X`),`source()` 逐级再拼会把同一段文本重复一遍。折叠语义
/// = **相同文本段只保留一次**:待拼段文本若已作为完整形态出现在已拼文本
/// 尾部,则跳过(首段 Severity 前缀原样保留);仅折叠 Display 拼接面,
/// 错误结构化面(kind/severity 载荷)零改动。
/// bin 目标是独立 crate,故必须 pub 而非 pub(crate)。
pub fn error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut text = err.to_string();
    let mut current = err.source();
    while let Some(cause) = current {
        let cause_text = cause.to_string();
        if !text.ends_with(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        current = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 链式测试替身(仅测试,无生产形态):固定 Display 文本 + 可选 source。
    #[derive(Debug)]
    struct ChainError {
        text: String,
        source: Option<Box<dyn std::error::Error + 'static>>,
    }

    impl ChainError {
        /// 叶子错误(无 source)。
        fn leaf(text: &str) -> Box<Self> {
            Box::new(ChainError {
                text: text.to_owned(),
                source: None,
            })
        }

        /// 带单级 source 的错误。
        fn with_source(text: &str, source: Box<dyn std::error::Error + 'static>) -> Box<Self> {
            Box::new(ChainError {
                text: text.to_owned(),
                source: Some(source),
            })
        }
    }

    impl fmt::Display for ChainError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(&self.text)
        }
    }

    impl std::error::Error for ChainError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.source.as_ref().map(|b| &**b as _)
        }
    }

    /// 折叠契约(卡内 ① 单段不变):无 source 的单段错误,Display 原样返回。
    #[test]
    fn error_chain_single_segment_unchanged() {
        let leaf = ChainError::leaf("readdirent /tmp/x: not a directory");
        assert_eq!(error_chain(&*leaf), "readdirent /tmp/x: not a directory");
    }

    /// 折叠契约(卡内 ① 无重复不变):链上各段文本互不重复 → 逐段拼接
    /// 语义与既有行为逐字节一致(零回归)。
    #[test]
    fn error_chain_without_duplicates_unchanged() {
        let chain = ChainError::with_source(
            "engine installer (fatal): extract failed",
            ChainError::with_source(
                "zip entry missing: rcd",
                ChainError::leaf("unexpected end of archive"),
            ),
        );
        assert_eq!(
            error_chain(&*chain),
            "engine installer (fatal): extract failed: \
             zip entry missing: rcd: unexpected end of archive"
        );
    }

    /// 折叠契约(卡内 ① 重复段折叠):D2 dogfood 现场同形——顶层 Display
    /// 已内嵌完整根因链文本,`source()` 逐级重复出现同段 → 折叠为一次,
    /// 首段 Severity 前缀(`retryable: `)保留。
    #[test]
    fn error_chain_folds_embedded_duplicate_chain() {
        let chain = ChainError::with_source(
            "retryable: engine installer (retryable): rc server error \
             (operations/list, http 500): error in ListJSON: \
             failed to read directory entry: readdirent /tmp/x: not a directory",
            ChainError::with_source(
                "engine installer (retryable): rc server error \
                 (operations/list, http 500): error in ListJSON: \
                 failed to read directory entry: readdirent /tmp/x: not a directory",
                ChainError::leaf("readdirent /tmp/x: not a directory"),
            ),
        );
        assert_eq!(
            error_chain(&*chain),
            "retryable: engine installer (retryable): rc server error \
             (operations/list, http 500): error in ListJSON: \
             failed to read directory entry: readdirent /tmp/x: not a directory"
        );
    }

    /// 折叠契约(卡内 ① 重复段折叠):相邻段文本完全相同 → 只保留一次。
    #[test]
    fn error_chain_folds_adjacent_equal_segments() {
        let chain = ChainError::with_source(
            "engine slot missing",
            ChainError::leaf("engine slot missing"),
        );
        assert_eq!(error_chain(&*chain), "engine slot missing");
    }

    /// 折叠契约(边界):仅后缀部分重叠(非完整段形态)不折叠——待拼段是
    /// 独立根因,信息不得吞掉(零吞错红线)。
    #[test]
    fn error_chain_keeps_suffix_overlapping_but_distinct_segment() {
        let chain = ChainError::with_source(
            "failed to read directory entry: not a directory",
            ChainError::leaf("directory entry closed"),
        );
        assert_eq!(
            error_chain(&*chain),
            "failed to read directory entry: not a directory: directory entry closed"
        );
    }
}
