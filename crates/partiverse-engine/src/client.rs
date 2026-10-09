//! rc 命令白名单客户端(M1-WP03-T01):对运行中槽位的 unix socket 以随机凭据
//! 发送 rc JSON 命令(POST),泛化 rcd.rs 探活通道的连接要素,零新增第三方依赖
//! (HTTP/1.1 最小客户端由 std UnixStream 手写)。白名单 = 架构 §3 清单剔除 V2 项
//! (卡内 ② 编译期枚举,清单见 [`RcMethod`] 宏单源);白名单外方法经
//! [`RcMethod::try_from_name`] 本地拒绝(Fatal,错误信息含被拒方法名),任意
//! 字符串零透传。错误映射(卡内 ③):`{error,input}`
//! 与 HTTP 状态 → 429/503=`RcThrottled`(Retryable,交 T03 预算器退避,不抛 UI)/
//! 5xx=`RcServerError`(Retryable)/其余非 2xx=`RcClientError`(Fatal);单次调用
//! 超时默认 10s 可配置,防僵死。协议形状经本机 rclone 1.75.1 实测核实。同步阻塞
//! API(ADR-0004):async 集成层调用方放 spawn_blocking。

use std::fmt;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use partiverse_core::error::Severity;

use crate::error::{EngineError, EngineErrorKind};

/// 单次 rc 调用超时默认值(卡内 ③:10s,防僵死;`with_timeout` 可配置)。
const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// 状态行/单行 framing 上限(超限 = 协议异常,防异常服务端刷内存)。
const MAX_LINE_BYTES: usize = 64 * 1024;
/// 错误详情截断长度(与 rcd.rs `first_line` 同口径)。
const DETAIL_MAX_CHARS: usize = 200;

/// rc 命令白名单单源宏:变体/canonical 名/全集同源生成,漂移在编译期不可能。
macro_rules! rc_methods {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        /// rc 命令白名单(卡内 ② 编译期枚举;架构 §3 清单剔除 V2 项。消费方:
        /// config/`=WP04 连接管理、operations/`=WP05-07 文件面、job|core/`=
        /// T02/T03 任务与预算、sync/`=WP06 传输。字符串透传面仅 canonical 名)。
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum RcMethod {
            $($variant,)+
        }

        impl RcMethod {
            /// 白名单全集(canonical 名与变体一一对应,`try_from_name` 数据源)。
            const ALL: &'static [RcMethod] = &[$(Self::$variant),+];

            /// canonical 命令名(请求行路径与日志的唯一字符串形态)。
            #[must_use]
            pub fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)+
                }
            }
        }
    };
}

rc_methods!(
    ConfigCreate => "config/create", ConfigUpdate => "config/update", ConfigGet => "config/get",
    ConfigDelete => "config/delete", ConfigDump => "config/dump", ConfigProviders => "config/providers",
    OperationsList => "operations/list", OperationsStat => "operations/stat", OperationsAbout => "operations/about",
    OperationsCopyfile => "operations/copyfile", OperationsMovefile => "operations/movefile", OperationsDelete => "operations/delete",
    OperationsMkdir => "operations/mkdir", OperationsUploadfile => "operations/uploadfile", OperationsPubliclink => "operations/publiclink",
    JobStatus => "job/status", JobList => "job/list", JobStop => "job/stop",
    CoreStats => "core/stats", CoreVersion => "core/version", CoreBwlimit => "core/bwlimit",
    CoreQuit => "core/quit", SyncCopy => "sync/copy", SyncMove => "sync/move",
);

impl RcMethod {
    /// 白名单解析门(卡内 ②:白名单外方法本地拒绝,Fatal,错误信息含被拒方法名;
    /// 任意字符串经此收口为枚举,零透传)。精确匹配 canonical 名,大小写敏感。
    pub fn try_from_name(name: &str) -> Result<Self, EngineError> {
        Self::ALL
            .iter()
            .find(|method| method.as_str() == name)
            .copied()
            .ok_or_else(|| {
                EngineError::new(
                    EngineErrorKind::RcMethodRejected(name.to_owned()),
                    Severity::Fatal,
                )
            })
    }
}

/// rc 白名单客户端:持有一次实例的连接要素(socket/随机凭据)与单次调用超时。
/// 手动实现 [`Debug`] 保证凭据脱敏(rcd.rs 同款纪律);随
/// [`crate::coordinator::EngineHandle`] 快照分发(T02/T03 消费面,卡内 ④)。
#[derive(Clone, PartialEq, Eq)]
pub struct RcClient {
    /// 本次实例的 unix socket 路径(仅 unix,卡内禁止 TCP)。
    socket_path: PathBuf,
    /// 随机认证用户(Debug 脱敏)。
    user: String,
    /// 随机认证口令(Debug 脱敏)。
    pass: String,
    /// 单次调用超时(默认 10s,可配置)。
    timeout: Duration,
}

impl fmt::Debug for RcClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RcClient")
            .field("socket_path", &self.socket_path)
            .field("user", &"<redacted>")
            .field("pass", &"<redacted>")
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl RcClient {
    /// 以连接要素构造客户端(超时取默认 10s;要素由监督器按实例随机生成)。
    #[must_use]
    pub fn new(socket_path: PathBuf, user: String, pass: String) -> Self {
        RcClient {
            socket_path,
            user,
            pass,
            timeout: DEFAULT_CALL_TIMEOUT,
        }
    }

    /// 配置单次调用超时(卡内 ③:可配置;builder 风格)。
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// 发送一条白名单内 rc 命令(POST JSON)并返回成功载荷;非 2xx 按状态映射
    /// 结构化错误(卡内 ③),2xx 载荷非法 JSON → Fatal `RcResponseInvalid`。
    pub fn call(
        &self,
        method: RcMethod,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, EngineError> {
        let (status, body) = self.execute(method.as_str(), params)?;
        if (200..300).contains(&status) {
            return serde_json::from_slice::<serde_json::Value>(&body).map_err(|err| {
                EngineError::new(
                    EngineErrorKind::RcResponseInvalid(format!(
                        "{}: non-JSON 2xx payload: {err}",
                        method.as_str()
                    )),
                    Severity::Fatal,
                )
            });
        }
        Err(classify_rc_error(method.as_str(), status, &body))
    }

    /// 一次完整调用:序列化 → 连接 → 写请求 → deadline 内读应答;IO 错误按
    /// 核心 `classify_io` 分级(网络瞬态 → Retryable)。
    fn execute(&self, method: &str, params: &serde_json::Value) -> Result<RcReply, EngineError> {
        let body = serde_json::to_vec(params).map_err(|err| {
            EngineError::new(
                EngineErrorKind::RcRequestInvalid(format!("serialize {method} params: {err}")),
                Severity::Fatal,
            )
        })?;
        // 单次调用预算 = 真实整体 deadline(卡内 ③ 防僵死,非「每读重置」)。
        let budget = CallBudget {
            deadline: Instant::now() + self.timeout,
            timeout: self.timeout,
        };
        // 卡内禁止 TCP:仅 UnixStream;连接失败按 classify_io 分级(socket 未就绪/
        // 引擎重启窗口 → ConnectionRefused/NotFound → Retryable)。
        let stream = UnixStream::connect(&self.socket_path).map_err(EngineError::from_io)?;
        stream
            .set_write_timeout(Some(self.timeout))
            .and_then(|_| stream.set_read_timeout(Some(self.timeout)))
            .map_err(EngineError::from_io)?;
        let request = render_request(method, &body, &basic_auth_value(&self.user, &self.pass));
        write_all_deadline(&stream, &request, &budget)?;
        read_reply(&stream, &budget)
    }
}

/// 单次调用的超时预算:整体 deadline + 配置值(`RcTimeout` 载荷锚定配置)。
struct CallBudget {
    deadline: Instant,
    timeout: Duration,
}

impl CallBudget {
    /// 剩余预算(到点/超支 → `RcTimeout` Retryable)。
    fn remaining(&self) -> Result<Duration, EngineError> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(EngineError::new(
                EngineErrorKind::RcTimeout {
                    timeout: self.timeout,
                },
                Severity::Retryable,
            ));
        }
        Ok(remaining)
    }
}

/// 一次 rc 调用的原始应答:(HTTP 状态,响应体字节[三种帧式归一后])。
type RcReply = (u16, Vec<u8>);

/// 渲染 HTTP/1.1 请求(POST /<method>,JSON 体,Basic 认证;Connection: close
/// 使响应帧式可预期,Host 为协议形式项、非网络端点)。
fn render_request(method: &str, body: &[u8], auth_value: &str) -> Vec<u8> {
    let mut request = format!(
        "POST /{method} HTTP/1.1\r\n\
         Host: localhost\r\n\
         Authorization: Basic {auth_value}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    )
    .into_bytes();
    request.extend_from_slice(body);
    request
}

/// Basic 认证头值:`base64(user:pass)`(RFC 4648 标准字母表 + padding;
/// std 无 base64,零新增依赖自实现,单测锚定向量)。
fn basic_auth_value(user: &str, pass: &str) -> String {
    encode_base64(format!("{user}:{pass}").as_bytes())
}

/// RFC 4648 标准 base64 编码(含 `=` padding;仅用于 Basic 头)。
fn encode_base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let bytes = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let group = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
        encoded.push(TABLE[((group >> 18) & 0x3f) as usize] as char);
        encoded.push(TABLE[((group >> 12) & 0x3f) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            TABLE[((group >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            TABLE[(group & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    encoded
}

/// deadline 内写全请求(短写续写;超时 → `RcTimeout`,其他 IO 按分级上浮)。
fn write_all_deadline(
    mut stream: &UnixStream,
    mut data: &[u8],
    budget: &CallBudget,
) -> Result<(), EngineError> {
    while !data.is_empty() {
        budget.remaining()?;
        let written = match stream.write(data) {
            // 信号打断(EINTR)= 非结果性中断:预算内重试(与 read 同口径)。
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            written => written.map_err(EngineError::from_io)?,
        };
        if written == 0 {
            return Err(EngineError::from_io(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "rc socket accepted zero bytes",
            )));
        }
        data = &data[written..];
    }
    Ok(())
}

/// deadline 内单次读取(超时 → `RcTimeout`;EINTR 预算内重试;EOF 由调用方
/// 按帧式语义裁决;其他 IO 按核心分级上浮)。
fn read_once(
    mut stream: &UnixStream,
    budget: &CallBudget,
    buffer: &mut [u8],
) -> Result<usize, EngineError> {
    loop {
        let remaining = budget.remaining()?;
        stream
            .set_read_timeout(Some(remaining))
            .map_err(EngineError::from_io)?;
        match stream.read(buffer) {
            Ok(n) => return Ok(n),
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Err(EngineError::new(
                    EngineErrorKind::RcTimeout {
                        timeout: budget.timeout,
                    },
                    Severity::Retryable,
                ));
            }
            // 信号打断(EINTR)= 非结果性中断:预算内重试,零吞错(并行测试与
            // 生产进程都会收到 SIGCHLD 等信号,阻塞读被戳断属常态)。
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(EngineError::from_io(err)),
        }
    }
}

/// deadline 内读满 `buffer`(中断帧合成 ConnectionReset → classify_io Retryable)。
fn read_exact_deadline(
    stream: &UnixStream,
    budget: &CallBudget,
    buffer: &mut [u8],
) -> Result<(), EngineError> {
    let mut filled = 0;
    while filled < buffer.len() {
        let n = read_once(stream, budget, &mut buffer[filled..])?;
        if n == 0 {
            return Err(EngineError::from_io(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "connection closed mid-frame",
            )));
        }
        filled += n;
    }
    Ok(())
}

/// deadline 内读一行(以 `\n` 结界,尾 `\r` 剥除;连接中断行 = 瞬态同上)。
fn read_line_deadline(stream: &UnixStream, budget: &CallBudget) -> Result<String, EngineError> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = read_once(stream, budget, &mut byte)?;
        if n == 0 {
            return Err(EngineError::from_io(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "connection closed mid-line",
            )));
        }
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
        if line.len() > MAX_LINE_BYTES {
            return Err(response_invalid("framing line exceeds limit"));
        }
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line).map_err(|err| response_invalid(format!("non-UTF8 line: {err}")))
}

/// 读取完整应答:帧式按 Content-Length / chunked / close 三分,deadline 内完成。
fn read_reply(stream: &UnixStream, budget: &CallBudget) -> Result<RcReply, EngineError> {
    let status_line = read_line_deadline(stream, budget)?;
    let status = parse_status_line(&status_line)?;
    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    let mut header_bytes = status_line.len() + 2;
    loop {
        let line = read_line_deadline(stream, budget)?;
        if line.is_empty() {
            break; // 空行 = 头终结
        }
        header_bytes += line.len() + 2;
        if header_bytes > MAX_LINE_BYTES {
            return Err(response_invalid("response headers exceed limit"));
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| response_invalid(format!("malformed header: {line}")))?;
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if name == "content-length" {
            content_length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| response_invalid(format!("bad content-length: {value}")))?,
            );
        } else if name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked") {
            chunked = true;
        }
    }
    let body = if chunked {
        read_chunked_body(stream, budget)?
    } else if let Some(length) = content_length {
        let mut body = vec![0u8; length];
        read_exact_deadline(stream, budget, &mut body)?;
        body
    } else {
        // close 定界(请求已声明 Connection: close 的兜底帧式)。
        read_to_deadline_eof(stream, budget)?
    };
    Ok((status, body))
}

/// 解析状态行 "HTTP/1.1 200 OK" → 200(协议异常 → Fatal)。
fn parse_status_line(line: &str) -> Result<u16, EngineError> {
    let mut parts = line.split_whitespace();
    let version = parts.next().unwrap_or("");
    let code = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/") {
        return Err(response_invalid(format!("bad status line: {line}")));
    }
    code.parse::<u16>()
        .map_err(|_| response_invalid(format!("bad status code in line: {line}")))
}

/// chunked 帧式读取(RFC 9112 §7.1;0 长度后吞 trailer 至空行;中断 = 瞬态)。
fn read_chunked_body(stream: &UnixStream, budget: &CallBudget) -> Result<Vec<u8>, EngineError> {
    let mut body = Vec::new();
    loop {
        let size_line = read_line_deadline(stream, budget)?;
        let size_text = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| response_invalid(format!("bad chunk size: {size_line}")))?;
        if size == 0 {
            // trailer 节直至空行(rclone 服务端不带 trailer,标准要求容错)。
            loop {
                if read_line_deadline(stream, budget)?.is_empty() {
                    return Ok(body);
                }
            }
        }
        let start = body.len();
        body.resize(start + size, 0);
        read_exact_deadline(stream, budget, &mut body[start..])?;
        let mut crlf = [0u8; 2];
        read_exact_deadline(stream, budget, &mut crlf)?;
        if crlf != *b"\r\n" {
            return Err(response_invalid("chunk data not terminated by CRLF"));
        }
    }
}

/// close 定界读取(至 EOF;EOF 即成功终点)。
fn read_to_deadline_eof(stream: &UnixStream, budget: &CallBudget) -> Result<Vec<u8>, EngineError> {
    let mut body = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let n = read_once(stream, budget, &mut buffer)?;
        if n == 0 {
            return Ok(body);
        }
        body.extend_from_slice(&buffer[..n]);
    }
}

/// `RcResponseInvalid` 构造(Fatal:完整但非法 = 协议被破坏;截断瞬态走 Io)。
fn response_invalid(detail: impl Into<String>) -> EngineError {
    EngineError::new(
        EngineErrorKind::RcResponseInvalid(detail.into()),
        Severity::Fatal,
    )
}

/// rc 错误响应结构化映射(卡内 ③,fixture 单测锚点):`error` 字段入 detail,
/// `input` 回显与调用方已发参数同源、不重复携带;零吞错(error 缺失或体非
/// JSON 时退回原始体截断,状态码与体片段均可见)。
fn classify_rc_error(method: &str, status: u16, body: &[u8]) -> EngineError {
    #[derive(serde::Deserialize)]
    struct RcError {
        #[serde(default)]
        error: Option<String>,
    }
    let parsed_error = serde_json::from_slice::<RcError>(body)
        .ok()
        .and_then(|parsed| parsed.error)
        .unwrap_or_else(|| {
            String::from_utf8_lossy(body)
                .chars()
                .take(DETAIL_MAX_CHARS)
                .collect()
        });
    let (kind, severity) = if status == 429 || status == 503 {
        (
            EngineErrorKind::RcThrottled {
                method: method.to_owned(),
                status,
                detail: parsed_error,
            },
            Severity::Retryable,
        )
    } else if (500..600).contains(&status) {
        (
            EngineErrorKind::RcServerError {
                method: method.to_owned(),
                status,
                detail: parsed_error,
            },
            Severity::Retryable,
        )
    } else {
        (
            EngineErrorKind::RcClientError {
                method: method.to_owned(),
                status,
                detail: parsed_error,
            },
            Severity::Fatal,
        )
    };
    EngineError::new(kind, severity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::EngineErrorKind as Kind;

    /// 卡内 ⑤ fixture:本机 rclone 1.75.1 实测错误体形状(三态同构)。
    const FIXTURE_400: &str = "{\n\t\"error\": \"Didn't find key \\\"name\\\" in input\",\n\t\"input\": {\n\t\t\"{}\": \"\"\n\t},\n\t\"path\": \"config/get\",\n\t\"status\": 400\n}";
    const FIXTURE_429: &str = "{\n\t\"error\": \"429 rate limit exceeded\",\n\t\"input\": {},\n\t\"path\": \"operations/list\",\n\t\"status\": 429\n}";
    const FIXTURE_500: &str = "{\n\t\"error\": \"didn't find section in config file (\\\"no-such\\\")\",\n\t\"input\": {\"srcFs\": \"no-such:\"},\n\t\"path\": \"sync/copy\",\n\t\"status\": 500\n}";

    #[test]
    fn whitelist_accepts_exact_canonical_names_and_rejects_off_list_locally() {
        // 编译期枚举 ↔ canonical 名全量往返(保真防漏项);白名单外本地拒绝
        // (Fatal,错误信息含被拒方法名);覆盖 V2 剔除项、禁透传项、大小写与
        // 尾随空白。
        for method in RcMethod::ALL {
            assert_eq!(
                RcMethod::try_from_name(method.as_str())
                    .unwrap_or_else(|err| panic!("{} must parse: {err}", method.as_str())),
                *method
            );
        }
        for rejected in [
            "core/command",
            "sync/bisync",
            "Core/Version",
            "core/version ",
            "",
        ] {
            let err = RcMethod::try_from_name(rejected).expect_err(rejected);
            assert_eq!(err.severity(), Severity::Fatal, "{rejected}");
            assert!(matches!(err.kind(), Kind::RcMethodRejected(name) if name == rejected));
            assert!(err.to_string().contains(rejected), "{err}");
        }
    }

    #[test]
    fn error_mapping_fixtures_match_card_severities() {
        // 卡内 ⑤:429/500/400 fixture 三例(不起真实服务)+ 边界类:503/502 →
        // Retryable;404 → Fatal;401 非 JSON 体退回原始体截断(零吞错)。
        let throttled = classify_rc_error("operations/list", 429, FIXTURE_429.as_bytes());
        assert_eq!(throttled.severity(), Severity::Retryable);
        let Kind::RcThrottled { method, status, .. } = throttled.kind() else {
            panic!("429 must be RcThrottled, got {:?}", throttled.kind())
        };
        assert_eq!((method.as_str(), *status), ("operations/list", 429));
        let Kind::RcThrottled { status: 503, .. } =
            classify_rc_error("core/stats", 503, FIXTURE_429.as_bytes()).kind()
        else {
            panic!("503 must be RcThrottled")
        };
        let server = classify_rc_error("sync/copy", 500, FIXTURE_500.as_bytes());
        assert_eq!(server.severity(), Severity::Retryable);
        let Kind::RcServerError { method, status, .. } = server.kind() else {
            panic!("5xx must be RcServerError, got {:?}", server.kind())
        };
        assert_eq!((method.as_str(), *status), ("sync/copy", 500));
        let Kind::RcServerError { status: 502, .. } =
            classify_rc_error("core/stats", 502, b"{}").kind()
        else {
            panic!("502 must be RcServerError")
        };
        let client = classify_rc_error("config/get", 400, FIXTURE_400.as_bytes());
        assert_eq!(client.severity(), Severity::Fatal);
        let Kind::RcClientError { method, status, .. } = client.kind() else {
            panic!("4xx must be RcClientError, got {:?}", client.kind())
        };
        assert_eq!((method.as_str(), *status), ("config/get", 400));
        assert!(client.to_string().contains("Didn't find key"));
        let unauthorized = classify_rc_error("core/version", 401, b"401 Unauthorized");
        assert_eq!(unauthorized.severity(), Severity::Fatal);
        assert!(unauthorized.to_string().contains("401 Unauthorized"));
    }

    #[test]
    fn basic_auth_request_shape_and_debug_redaction_hold() {
        // RFC 4648 锚定向量 + 请求行/头形态 + 凭据 Debug 脱敏与超时可配置。
        assert_eq!(encode_base64(b"foo"), "Zm9v");
        assert_eq!(encode_base64(b"foob"), "Zm9vYg==");
        assert_eq!(encode_base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode_base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(basic_auth_value("user", "pass"), "dXNlcjpwYXNz");
        let text = String::from_utf8(render_request("core/version", b"{}", "dXNlcjpwYXNz"))
            .expect("request is UTF-8");
        assert!(text.starts_with("POST /core/version HTTP/1.1\r\n"));
        assert!(text.contains("Content-Length: 2\r\n"));
        assert!(text.contains("Authorization: Basic dXNlcjpwYXNz\r\n"));
        assert!(text.contains("Content-Type: application/json\r\n"));
        assert!(text.ends_with("\r\n\r\n{}"));
        let client = RcClient::new(
            PathBuf::from("/tmp/pv.sock"),
            "secret-user".into(),
            "secret-pass".into(),
        );
        let dump = format!("{client:?}");
        assert!(!dump.contains("secret-user") && !dump.contains("secret-pass"));
        assert_eq!(client.timeout, DEFAULT_CALL_TIMEOUT);
        assert_eq!(
            client.with_timeout(Duration::from_secs(3)).timeout,
            Duration::from_secs(3)
        );
    }

    // —— Fake* 帧式/超时单测(仅测试且命名 Fake*):std UnixListener 手写
    // 应答器,验证三帧式与整体 deadline,不触真实网络。

    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Fake 服务端:临时目录 unix socket,accept 一次连接交测试体处置。
    struct FakeRcServer {
        listener: UnixListener,
        path: PathBuf,
    }

    impl FakeRcServer {
        /// 启动(pid+原子计数唯一化 socket 名,防同进程重复测试撞名)。
        fn start(tag: &str) -> Self {
            static SEQ: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "partiverse-fake-rc-{tag}-{}-{}.sock",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_file(&path);
            let listener = UnixListener::bind(&path).expect("fake rc server binds");
            FakeRcServer { listener, path }
        }

        /// accept 一次连接(EINTR 重试;测试路径允许 panic)。
        fn accept_once(&self) -> UnixStream {
            loop {
                match self.listener.accept() {
                    Ok((stream, _addr)) => return stream,
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(err) => panic!("fake rc server accepts: {err}"),
                }
            }
        }
    }

    impl Drop for FakeRcServer {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// 读尽客户端请求(一发即等,读到短超时静默即完整;EINTR 重试)。
    fn fake_read_request(mut stream: &UnixStream) -> Vec<u8> {
        stream
            .set_read_timeout(Some(Duration::from_millis(50)))
            .expect("fake sets read timeout");
        let mut request = Vec::new();
        loop {
            let mut buffer = [0u8; 4096];
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => request.extend_from_slice(&buffer[..n]),
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err)
                    if err.kind() == std::io::ErrorKind::WouldBlock
                        || err.kind() == std::io::ErrorKind::TimedOut =>
                {
                    break;
                }
                Err(err) => panic!("fake reads request: {err}"),
            }
        }
        request
    }

    #[test]
    fn fake_server_serves_all_three_framings_and_auth_header() {
        // 三帧式逐一归一为 JSON 载荷;凭据以 Basic 头到达服务端(端到端形状)。
        let server = FakeRcServer::start("frames");
        let socket = server.path.clone();
        let expected_auth = basic_auth_value("fake-user", "fake-pass");
        let handle = std::thread::spawn(move || {
            let mut stream = server.accept_once();
            let request = fake_read_request(&stream);
            let head = String::from_utf8_lossy(&request).into_owned();
            assert!(
                head.starts_with("POST /core/version HTTP/1.1\r\n"),
                "{head}"
            );
            assert!(head.contains(&format!("Authorization: Basic {expected_auth}\r\n")));

            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\n\r\n")
                .and_then(|_| stream.write_all(br#"{"version":"v1"}"#))
                .expect("fake writes content-length reply");
            drop(stream);
            let mut stream = server.accept_once();
            let _ = fake_read_request(&stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .and_then(|_| stream.write_all(b"c\r\n{\"jobid\": 1}\r\n0\r\n\r\n"))
                .expect("fake writes chunked reply");
            drop(stream);
            // close 定界:无 Content-Length/chunked,写完即关(EOF 定界)。
            let mut stream = server.accept_once();
            let _ = fake_read_request(&stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\n\r\n{}")
                .expect("fake writes close-delimited reply");
        });
        let client = RcClient::new(socket, "fake-user".into(), "fake-pass".into());
        let version = client
            .call(RcMethod::CoreVersion, &serde_json::json!({}))
            .expect("content-length frame parses");
        assert_eq!(version, serde_json::json!({"version": "v1"}));
        let job = client
            .call(RcMethod::JobList, &serde_json::json!({}))
            .expect("chunked frame parses");
        assert_eq!(job, serde_json::json!({"jobid": 1}));
        let stats = client
            .call(RcMethod::CoreStats, &serde_json::json!({}))
            .expect("close-delimited frame parses");
        assert_eq!(stats, serde_json::json!({}));
        handle.join().expect("fake server thread");
    }

    #[test]
    fn fake_server_stall_times_out_as_retryable_within_budget() {
        // 卡内 ③:僵死服务端在整体预算内超时(Retryable RcTimeout),不悬挂。
        let server = FakeRcServer::start("stall");
        let socket = server.path.clone();
        let handle = std::thread::spawn(move || {
            let stream = server.accept_once();
            let _ = fake_read_request(&stream);
            // 装死(不回字节、不关),直至 client 侧超时断链。
            std::thread::sleep(Duration::from_secs(2));
        });
        let client =
            RcClient::new(socket, "u".into(), "p".into()).with_timeout(Duration::from_millis(150));
        let err = client
            .call(RcMethod::CoreVersion, &serde_json::json!({}))
            .expect_err("stalled server must time out within budget");
        assert_eq!(err.severity(), Severity::Retryable);
        assert!(matches!(
            err.kind(),
            Kind::RcTimeout { timeout } if *timeout == Duration::from_millis(150)
        ));
        handle.join().expect("fake server thread");
    }

    #[test]
    fn fake_server_truncated_reply_maps_to_retryable_io() {
        // 截断应答(连接中断帧)= 瞬态:合成 ConnectionReset → classify_io Retryable。
        let server = FakeRcServer::start("truncated");
        let socket = server.path.clone();
        let handle = std::thread::spawn(move || {
            let mut stream = server.accept_once();
            let _ = fake_read_request(&stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\n\r\nshort")
                .expect("fake writes truncated reply");
            drop(stream);
        });
        let client = RcClient::new(socket, "u".into(), "p".into());
        let err = client
            .call(RcMethod::CoreVersion, &serde_json::json!({}))
            .expect_err("truncated reply must fail");
        assert_eq!(err.severity(), Severity::Retryable);
        assert!(matches!(err.kind(), Kind::Io), "{:?}", err.kind());
        handle.join().expect("fake server thread");
    }
}
