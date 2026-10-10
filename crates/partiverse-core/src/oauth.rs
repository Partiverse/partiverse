//! OAuth 本地回调服务器与百度 oob 式本地换码(M1-WP04-T02,R2 凭据钉子)。
//!
//! 构成(卡内 DoD):①[`LocalCallbackServer`]=127.0.0.1 随机端口,std
//! TcpListener 与手写最小 HTTP(engine client.rs 同风格,零新增依赖),按次
//! 授权启停(Drop 即停),state 精确匹配防 CSRF;②[`BaiduOAuthFlow`]=authorize
//! URL 构造 → 用户回填 code → **本地** GET token 端点换码(client 四件套由
//! 调用方从用户侧配置注入,禁入 git/硬编码)→ token 经 [`OAuthTokenSink`]
//! 下沉交 rclone 写加密 config,本模块**零 token 持久化**(架构 §5:凭据单一
//! 真相源=加密 config+keychain,刷新 token 由 rclone 原子写回);③零明文面=
//! Debug 全脱敏、错误载荷白名单、provider 文本经 [`redact`] 脱敏(卡内 ⑦)。
//!
//! # rclone authorize 复用性核实(卡内 ③;2026-10-10 实机+内嵌官方文档,
//! 宿主件=引擎 manifest 钉定同版上游发行件 rclone v1.75.1)
//! - **CLI 面**:`rclone authorize <backendname> [blob | client_id client_secret]`
//!   存在(authorize --help 实测),流程=rclone 内建 OAuth 服务器(README.txt:
//!   19479;config/oauthstatus 文档示例 authUrl=`http://127.0.0.1:53682/auth?
//!   state=...`,README.txt:19515-19516——审查 [low] 修正原误记 21458-21470,
//!   该区间实为 config/oauthstatus rc 文档),且须已注册 backend:实测
//!   `rclone authorize baidu` → `didn't find backend called "baidu"`;
//!   `help backends` 无 baidu 系条目(百度 Go 后端属 backends-go fork 路线,
//!   未随官方 1.75.1 发行)。
//! - **rc 面**:仅 `config/oauthstatus`/`config/oauthstop`(README.txt:21458/
//!   21472)与 `config create --non-interactive` 的 `*oauth-*` 状态机
//!   (README.txt:3816),均驱动同一内建 OAuth 服务器,同样依赖注册 backend+
//!   localhost 回调,无「code 换 token」通用方法。
//! - **协议不合**:百度回调地址须控制台预登记**公网域名**(baidu.md:10),
//!   localhost 不可登记;oob 式=授权页展示 code、用户复制回填(baidu.md:26,29),
//!   rclone 内建面无此入口。**结论**:两路径均不可复用 → 本卡自实现本地回调+
//!   oob 回填+本地换码;token 下沉经 [`OAuthTokenSink`] 由引擎/应用层写 rclone
//!   加密 config(本卡清单仅 core,trait 接缝同 T01 `RcDispatch` 先例)。

use std::fmt;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use crate::credential_store::{random_bytes, redact};
use crate::error::{PartisyError, Severity, classify_io};

/// 百度授权端点(baidu.md:21;浏览器导航面,非本方 HTTP 客户端请求面)。
pub const BAIDU_AUTHORIZE_ENDPOINT: &str = "http://openapi.baidu.com/oauth/2.0/authorize";
/// 百度 token 端点(baidu.md:23;本方 HTTP 客户端唯一生产请求面,https)。
pub const BAIDU_TOKEN_ENDPOINT: &str = "https://openapi.baidu.com/oauth/2.0/token";
/// scope 固定值(baidu.md:21;Implicit 流 30 天不可刷新,禁用,见 baidu.md:26)。
pub const BAIDU_SCOPE: &str = "basic,netdisk";
/// oob 模式 redirect_uri 固定值(baidu.md:26;token 请求须回带同值,baidu.md:23)。
pub const BAIDU_OOB_REDIRECT_URI: &str = "oob";
/// 自建 client 引导文案 key(卡内 ④;资源化与 UI 接线属 WP05,GUI 唯一语言=English)。
pub const CLIENT_CREDENTIALS_GUIDANCE_KEY: &str = "oauth.client-credentials.missing";
/// 引导文案英文正文(与 key 同源交付;自建 client 口径同 GDrive drive.file D7)。
pub const CLIENT_CREDENTIALS_GUIDANCE_EN: &str = "No OAuth client is configured. Create your own app on the provider open platform, then provide its client_id and client_secret in connection settings.";
/// 百度沙箱提示文案 key(卡内边界项;审查 [medium] 修正补齐;baidu.md:14,52)。
pub const BAIDU_SANDBOX_NOTICE_KEY: &str = "oauth.baidu.sandbox-scope-notice";
/// 提示英文正文:未审核应用 10 次/小时限审+可访问范围限 /apps/{appname} 沙箱。
pub const BAIDU_SANDBOX_NOTICE_EN: &str = "Unverified Baidu apps are rate-limited during review, and access is restricted to the /apps/{appname} sandbox directory.";

/// 结构化错误类别(载荷白名单见变体,一律不得携带 code/secret/token——卡内 ⑦)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthErrorKind {
    /// 回调 state 与会话预期不符(CSRF 防线,失败即拒绝整次授权,Fatal)。
    StateMismatch,
    /// 授权方回调显式 error= 参数(载荷=provider error 码,经 state 脱敏+截断)。
    ProviderDenied(String),
    /// 授权等待或 token 交换超时(Retryable,用户可重新发起)。
    Timeout,
    /// client 凭据缺省(载荷=引导文案 key,禁内嵌凭据,卡内 ④,Fatal)。
    ClientCredentialsMissing(&'static str),
    /// 输入不合法(空 code/state、端点协议不符;载荷=结构性描述,Fatal)。
    Invalid(String),
    /// token 端点非 2xx(载荷=状态码数字,零响应体——体可能回显请求参数)。
    ExchangeHttpStatus(u16),
    /// 2xx 响应不合法(非 JSON/缺字段/携带 error/超限;载荷=脱敏诊断,Fatal)。
    ExchangeResponseInvalid(String),
    /// 传输失败(载荷仅 Io 原文或固定措辞——ureq 其余变体 Display 可能内嵌
    /// 请求 URL,而 URL query 携带凭据,细节一律不入错误,卡内 ⑦)。
    ExchangeTransport(String),
    /// 本地回调监听/读写 IO(severity 由 [`classify_io`] 表驱动)。
    Io,
}

impl fmt::Display for OAuthErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OAuthErrorKind::StateMismatch => write!(f, "callback state mismatch (possible CSRF)"),
            OAuthErrorKind::ProviderDenied(r) => write!(f, "authorization denied by provider: {r}"),
            OAuthErrorKind::Timeout => write!(f, "authorization timed out"),
            OAuthErrorKind::ClientCredentialsMissing(k) => {
                write!(f, "client credentials missing (guidance: {k})")
            }
            OAuthErrorKind::Invalid(d) => write!(f, "invalid oauth input: {d}"),
            OAuthErrorKind::ExchangeHttpStatus(s) => write!(f, "token endpoint http {s}"),
            OAuthErrorKind::ExchangeResponseInvalid(d) => {
                write!(f, "token endpoint response invalid: {d}")
            }
            OAuthErrorKind::ExchangeTransport(d) => {
                write!(f, "token endpoint transport failure: {d}")
            }
            OAuthErrorKind::Io => write!(f, "io error"),
        }
    }
}

impl std::error::Error for OAuthErrorKind {}

impl OAuthErrorKind {
    /// Fatal 构造(kind 挂 [`PartisyError::source`],credential_store 同款)。
    fn fatal(self) -> PartisyError {
        PartisyError {
            severity: Severity::Fatal,
            source: Some(Box::new(self)),
        }
    }

    /// Retryable 构造(超时/传输抖动等可重试类)。
    fn retryable(self) -> PartisyError {
        PartisyError {
            severity: Severity::Retryable,
            source: Some(Box::new(self)),
        }
    }

    /// 状态码表驱动定级(408/429/5xx=瞬态可重试交上层退避,余 Fatal)。
    fn with_status_severity(self, status: u16) -> PartisyError {
        let retryable = matches!(status, 408 | 429 | 500..=599);
        self.with_severity(if retryable {
            Severity::Retryable
        } else {
            Severity::Fatal
        })
    }

    /// 显式 severity 构造。
    fn with_severity(self, severity: Severity) -> PartisyError {
        PartisyError {
            severity,
            source: Some(Box::new(self)),
        }
    }
}

/// 从 [`PartisyError`] 还原结构化类别(非本模块来源的错误 → None)。
#[must_use]
pub fn kind_of(err: &PartisyError) -> Option<&OAuthErrorKind> {
    err.source
        .as_ref()
        .and_then(|s| s.downcast_ref::<OAuthErrorKind>())
}

/// IO 错误构造(severity 由核心 `classify_io` 表驱动,credential_store 同款)。
fn io_err(err: std::io::Error) -> PartisyError {
    PartisyError {
        severity: classify_io(err.kind()),
        source: Some(Box::new(err)),
    }
}

/// OAuth client 凭据(卡内 ④):四件套中 OAuth 交换所需一双,由调用方从用户侧
/// 配置文件注入(AppID/SignKey 等其余件由 Go 后端面消费,不经本模块)。
#[derive(Clone, PartialEq, Eq)]
pub struct ClientCredentials {
    /// client_id(授权 URL 与 token 请求 query 使用;非 ⑦ 清单项,Debug 可见)。
    pub client_id: String,
    client_secret: String,
}

impl ClientCredentials {
    /// 非空校验(纯空白视同缺省)后构造。
    ///
    /// # Errors
    /// 缺省 → [`OAuthErrorKind::ClientCredentialsMissing`](Fatal,卡内 ④)。
    pub fn new(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
    ) -> Result<Self, PartisyError> {
        let (client_id, client_secret) = (client_id.into(), client_secret.into());
        if client_id.trim().is_empty() || client_secret.trim().is_empty() {
            return Err(
                OAuthErrorKind::ClientCredentialsMissing(CLIENT_CREDENTIALS_GUIDANCE_KEY).fatal(),
            );
        }
        Ok(ClientCredentials {
            client_id,
            client_secret,
        })
    }

    /// client_secret(仅 token 请求 query 使用;Debug/错误零出现)。
    fn client_secret(&self) -> &str {
        &self.client_secret
    }
}

impl fmt::Debug for ClientCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientCredentials")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .finish()
    }
}

/// token 端点成功响应最小集(baidu.md:24-25:access_token 30 天可刷新;
/// refresh_token 10 年**一次性**,必须保存刷新响应中的新值——两枚原样携带,
/// 刷新与持久化语义归 rclone 写回加密 config,架构 §5)。
#[derive(Clone, PartialEq, Eq)]
pub struct TokenSet {
    /// access_token(30 天,baidu.md:24;Debug 脱敏)。
    pub access_token: String,
    /// refresh_token(10 年一次性,baidu.md:25;交 rclone 后由其写回更新)。
    pub refresh_token: String,
    /// access_token 有效期(秒)。
    pub expires_in: u64,
}

impl fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// token 下沉接缝(卡内 ②「token 交 rclone 写入加密 config」):本模块零
/// token 持久化,生产实现由引擎/应用层经 rc `config/update`/远端创建注入
/// rclone 加密 config(架构 §5「凭据单一真相源」;trait 接缝同 T01
/// `RcDispatch` 先例,UI/引擎接线属 WP05)。
pub trait OAuthTokenSink {
    /// 写入一次授权所得 token;失败按实现方语义显式上浮(零吞错)。
    fn store_token(&self, token: &TokenSet) -> Result<(), PartisyError>;
}

/// 生成 OAuth state(CSRF 防护,卡内 ①):CSPRNG(与主密钥同源)取 16 字节 →
/// 32 位小写 hex(128 bit 熵;hex 对 URL/query 全安全)。
///
/// # Errors
/// 随机源不可用 → 结构化 Fatal(同主密钥生成口径)。
pub fn generate_state() -> Result<String, PartisyError> {
    let bytes = random_bytes()?;
    Ok(bytes
        .iter()
        .take(16)
        .fold(String::with_capacity(32), |mut acc, byte| {
            use std::fmt::Write as _;
            let _ = write!(acc, "{byte:02x}");
            acc
        }))
}

/// 百度 oob 式授权流(卡内 ②):authorize URL 构造 + 本地 GET 换码;端点与
/// scope 固定自 provider 知识库(模块头核实节)。
pub struct BaiduOAuthFlow {
    credentials: ClientCredentials,
    agent: ureq::Agent,
    token_endpoint: String,
    exchange_timeout: Duration,
}

impl BaiduOAuthFlow {
    /// 生产流:官方 https token 端点 + 默认 30s 交换超时;代理跟随环境变量
    /// (ureq 默认读 ALL_PROXY/HTTPS_PROXY/HTTP_PROXY,源码 config.rs:948)。
    #[must_use]
    pub fn new(credentials: ClientCredentials) -> Self {
        BaiduOAuthFlow {
            credentials,
            agent: agent_with(Duration::from_secs(30), false),
            token_endpoint: BAIDU_TOKEN_ENDPOINT.to_owned(),
            exchange_timeout: Duration::from_secs(30),
        }
    }

    /// 替换 token 端点(测试隔离面,同 `KeyringStore::with_names` 先例):仅
    /// 接受 https 或**回环 http**(卡内 ⑥ 禁真实网络,Fake 注入缝);回环端点
    /// 显式剥离 env 代理,防 HTTP_PROXY 把 127.0.0.1 测试路由进代理(ureq
    /// 默认自动读环境代理,config.rs:948)。须在 [`Self::with_exchange_timeout`]
    /// 前调用。
    ///
    /// # Errors
    /// 非 https 且非回环 http → [`OAuthErrorKind::Invalid`](Fatal;证书校验
    /// 不得关闭,ADR-0004,任何代码路径无关闭开关)。
    pub fn with_token_endpoint(mut self, endpoint: &str) -> Result<Self, PartisyError> {
        let loopback = is_loopback_http(endpoint);
        if !endpoint.starts_with("https://") && !loopback {
            return Err(OAuthErrorKind::Invalid(
                "token endpoint must be https (loopback http is reserved for local test fixtures)"
                    .into(),
            )
            .fatal());
        }
        if loopback {
            self.agent = agent_with(self.exchange_timeout, true);
        }
        self.token_endpoint = endpoint.to_owned();
        Ok(self)
    }

    /// 配置单次 token 交换超时(builder 风格,client.rs 同款)。
    #[must_use]
    pub fn with_exchange_timeout(mut self, timeout: Duration) -> Self {
        self.exchange_timeout = timeout;
        self.agent = agent_with(timeout, is_loopback_http(&self.token_endpoint));
        self
    }

    /// 构造百度 authorize URL(baidu.md:21 契约:`response_type=code` +
    /// client_id + `redirect_uri=oob`(baidu.md:26)+ scope 固定 + state;禁
    /// Implicit 流;授权页展示 code、用户复制回填,baidu.md:29)。允许 localhost
    /// 回调的 provider 改用 [`LocalCallbackServer::redirect_uri`]。
    ///
    /// # Errors
    /// state 为空 → [`OAuthErrorKind::Invalid`](Fatal;state 由
    /// [`generate_state`] 产生并交调用方会话保存)。
    pub fn authorize_url(&self, state: &str) -> Result<String, PartisyError> {
        if state.is_empty() {
            return Err(OAuthErrorKind::Invalid(
                "state must not be empty (generate via generate_state)".into(),
            )
            .fatal());
        }
        Ok(format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}",
            BAIDU_AUTHORIZE_ENDPOINT,
            percent_encode_query_value(self.credentials.client_id.as_str()),
            percent_encode_query_value(BAIDU_OOB_REDIRECT_URI),
            percent_encode_query_value(BAIDU_SCOPE),
            percent_encode_query_value(state),
        ))
    }

    /// 本地换码(卡内 ②,oob 回填接口:code+state → token):GET token 端点
    /// (baidu.md:23 契约:grant_type/code/client_id/client_secret/redirect_uri
    /// 五参数 query 携带;secret 只进本请求,零日志/错误/远端中转——「code 换
    /// token 必须本地完成」红线)。state 不入 token 请求(端点无该参数;校验面
    /// 在回调服务器,oob 回填路径由调用方持有比对)。
    ///
    /// # Errors
    /// 输入空 → `Invalid`;传输/超时 → `ExchangeTransport`/`Timeout`;
    /// 非 2xx → `ExchangeHttpStatus`;2xx 不合法 → `ExchangeResponseInvalid`。
    pub fn exchange_code(&self, code: &str, state: &str) -> Result<TokenSet, PartisyError> {
        if code.is_empty() || state.is_empty() {
            return Err(OAuthErrorKind::Invalid(
                "authorization code and state must not be empty".into(),
            )
            .fatal());
        }
        let url = format!(
            "{}?grant_type=authorization_code&code={}&client_id={}&client_secret={}&redirect_uri={}",
            self.token_endpoint,
            percent_encode_query_value(code),
            percent_encode_query_value(self.credentials.client_id.as_str()),
            percent_encode_query_value(self.credentials.client_secret()),
            percent_encode_query_value(BAIDU_OOB_REDIRECT_URI),
        );
        // 默认 http_status_as_error 开启:非 2xx 直接转 Error::StatusCode。
        let response = self.agent.get(&url).call().map_err(map_exchange_error)?;
        // ureq 3 的 Response::status() 返回 http::StatusCode,统一取 u16。
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            // 状态码入错误,响应体一律不入(体可能回显请求参数,卡内 ⑦)。
            return Err(OAuthErrorKind::ExchangeHttpStatus(status).with_status_severity(status));
        }
        // 响应体 64 KiB 封顶(官方响应 <1 KiB),防恶意端点刷内存。
        let body = response
            .into_body()
            .with_config()
            .limit(64 * 1024)
            .read_to_string()
            .map_err(|err| match err {
                ureq::Error::BodyExceedsLimit(_) => OAuthErrorKind::ExchangeResponseInvalid(
                    "token response exceeds 64 KiB cap".into(),
                )
                .fatal(),
                other => map_exchange_error(other),
            })?;
        parse_token_response(&body, code, self.credentials.client_secret())
    }
}

/// 构造 ureq Agent(全局超时强制;`no_proxy`=剥离环境代理,仅供回环 Fake 端点)。
fn agent_with(timeout: Duration, no_proxy: bool) -> ureq::Agent {
    let mut config = ureq::Agent::config_builder().timeout_global(Some(timeout));
    if no_proxy {
        config = config.proxy(None);
    }
    config.build().into()
}

/// 回环 http 端点判定(host 取 authority 段,容忍端口与 IPv6 方括号)。
fn is_loopback_http(endpoint: &str) -> bool {
    let Some(rest) = endpoint.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "127.0.0.1" || host == "localhost" || host == "::1"
}

/// ureq 传输错误 → 结构化映射(零 URL 泄漏,卡内 ⑦):超时与 Io 文本安全
/// (ureq error.rs:232-236,Display 不含 URL);其余变体(BadUri 等)Display
/// 可能内嵌请求 URL → 固定措辞(类型上浮仍显式,零吞错)。传输类失败按瞬态
/// Retryable,重试决策交上层与预算器。
fn map_exchange_error(err: ureq::Error) -> PartisyError {
    match err {
        ureq::Error::StatusCode(status) => {
            OAuthErrorKind::ExchangeHttpStatus(status).with_status_severity(status)
        }
        ureq::Error::Timeout(_) => OAuthErrorKind::Timeout.retryable(),
        ureq::Error::Io(io_error) => {
            OAuthErrorKind::ExchangeTransport(format!("io: {io_error}")).retryable()
        }
        _ => OAuthErrorKind::ExchangeTransport(
            "token endpoint transport failure (detail withheld: request url carries credentials)"
                .into(),
        )
        .retryable(),
    }
}

/// 解析 token 端点 2xx 响应:取 access_token/refresh_token/expires_in
/// (baidu.md:24-25);provider 2xx 错误体(error 字段)→ 结构化拒绝,载荷经
/// [`redact`] 以 code/secret 过滤并截断 100 字符(error_description 不取——
/// 自由文本可能回显请求参数;字段名入错误,值不入)。
fn parse_token_response(body: &str, code: &str, secret: &str) -> Result<TokenSet, PartisyError> {
    let invalid = |detail: String| OAuthErrorKind::ExchangeResponseInvalid(detail).fatal();
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|err| invalid(format!("non-JSON token response: {err}")))?;
    if let Some(provider_error) = value.get("error").and_then(serde_json::Value::as_str) {
        let mut detail = provider_error.to_owned();
        for secret_value in [code, secret] {
            detail = redact(&detail, secret_value);
        }
        return Err(invalid(format!(
            "token endpoint rejected exchange: {}",
            detail.chars().take(100).collect::<String>()
        )));
    }
    let field = |name: &str| {
        value
            .get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| invalid(format!("missing field: {name}")))
    };
    let expires_in = value
        .get("expires_in")
        .and_then(serde_json::Value::as_u64)
        .filter(|seconds| *seconds > 0)
        .ok_or_else(|| invalid("missing or invalid expires_in".into()))?;
    Ok(TokenSet {
        access_token: field("access_token")?,
        refresh_token: field("refresh_token")?,
        expires_in,
    })
}

/// query 分量百分号编码:RFC 3986 unreserved + `,`(baidu.md:21 的
/// `scope=basic,netdisk` 原样呈现,逗号属 query 合法子定界符),其余按 UTF-8
/// 大写 %XX。
fn percent_encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b',' => {
                out.push(*byte as char);
            }
            other => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

/// application/x-www-form-urlencoded 解码(`%XX` 与 `+`→空格);非法 `%`
/// 序列 → None(调用方按 fail-closed 处理)。
fn percent_decode_query_value(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let hex = bytes.get(index + 1..index + 3)?;
                let high = (hex[0] as char).to_digit(16)?;
                let low = (hex[1] as char).to_digit(16)?;
                out.push(((high * 16) + low) as u8);
                index += 3;
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// 本地回调服务器(卡内 ①):127.0.0.1 随机端口,按次授权启停(Drop 即停)。
#[derive(Debug)]
pub struct LocalCallbackServer {
    listener: std::net::TcpListener,
    local_addr: std::net::SocketAddr,
}

/// 单个回调连接的裁决(Noise=无授权结论的浏览器噪音/不可读连接,应答后继续等待)。
enum CallbackOutcome {
    Code(String),
    StateMismatch,
    Denied(String),
    Noise,
}

impl LocalCallbackServer {
    /// 绑定 127.0.0.1 随机端口(端口=0 由内核分配,不监听外网)。
    ///
    /// # Errors
    /// 监听失败按 [`classify_io`] 分级上浮。
    pub fn bind() -> Result<Self, PartisyError> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(io_err)?;
        let local_addr = listener.local_addr().map_err(io_err)?;
        Ok(LocalCallbackServer {
            listener,
            local_addr,
        })
    }

    /// 本机回调 redirect_uri(供允许 localhost 回调的 provider 使用;百度 oob
    /// 路径不经过本服务器)。
    #[must_use]
    pub fn redirect_uri(&self) -> String {
        format!("http://{}/callback", self.local_addr)
    }

    /// 等待一次授权回调(卡内 ①:state 精确匹配防 CSRF)。无授权结论的连接
    /// (favicon 等)应答 400 后继续等待;state 不符 → 拒绝整次授权(**fail
    /// closed**:疑似 CSRF/串话不与合法回调混流);provider 显式 error= →
    /// `ProviderDenied`;deadline 到 → Retryable `Timeout`(用户可重新发起)。
    /// 回执页静态英文,零 query 回显(卡内 ⑦)。
    ///
    /// # Errors
    /// 见 [`OAuthErrorKind`] 各变体;监听/读写 IO 按 [`classify_io`] 分级。
    pub fn wait_for_code(
        &self,
        expected_state: &str,
        timeout: Duration,
    ) -> Result<String, PartisyError> {
        if expected_state.is_empty() {
            return Err(OAuthErrorKind::Invalid("expected state must not be empty".into()).fatal());
        }
        let deadline = Instant::now() + timeout;
        self.listener.set_nonblocking(true).map_err(io_err)?;
        loop {
            if Instant::now() >= deadline {
                return Err(OAuthErrorKind::Timeout.retryable());
            }
            match self.listener.accept() {
                Ok((mut stream, _)) => {
                    // BSD 系 accept() 返回的 socket 继承监听端的 O_NONBLOCK
                    // (accept(2) 平台差异,Linux 不继承;审查 [medium]):恢复
                    // 阻塞模式,否则合法回调首读 EAGAIN 被当 Noise 丢弃。
                    stream.set_nonblocking(false).map_err(io_err)?;
                    // 授权结论由返回值传递;回执页为体验性收尾(写失败不影响已
                    // 取得的结论,stream 随即丢弃,非生产数据通路)。
                    match callback_outcome(&mut stream, expected_state, deadline) {
                        CallbackOutcome::Code(code) => {
                            write_html_page(&mut stream, 200, "OK", CALLBACK_SUCCESS_HTML);
                            return Ok(code);
                        }
                        CallbackOutcome::StateMismatch => {
                            write_html_page(&mut stream, 400, "Bad Request", CALLBACK_STATE_HTML);
                            return Err(OAuthErrorKind::StateMismatch.fatal());
                        }
                        CallbackOutcome::Denied(reason) => {
                            write_html_page(&mut stream, 400, "Bad Request", CALLBACK_DENIED_HTML);
                            return Err(OAuthErrorKind::ProviderDenied(reason).fatal());
                        }
                        CallbackOutcome::Noise => {
                            write_html_page(&mut stream, 400, "Bad Request", CALLBACK_NOISE_HTML);
                        }
                    }
                }
                // nonblocking 轮询:25ms 空转,deadline 精度与功耗折中。
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(err) => return Err(io_err(err)),
            }
        }
    }
}

/// 读取并裁决单个回调连接(读取限时:剩余 deadline 与 5s 取小;超时/残缺 →
/// Noise,不上升为授权结论)。
fn callback_outcome(
    stream: &mut TcpStream,
    expected_state: &str,
    deadline: Instant,
) -> CallbackOutcome {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return CallbackOutcome::Noise;
    }
    let _ = stream.set_read_timeout(Some(remaining.min(Duration::from_secs(5))));
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                // 头部终结符即达(授权回调无 body);超 64 KiB = 协议异常,防刷内存。
                if buffer.windows(4).any(|w| w == b"\r\n\r\n") || buffer.len() > 64 * 1024 {
                    break;
                }
            }
            // 连接读取失败(超时/对端重置等)= 无授权结论,继续等待直到
            // deadline(非授权失败,不上升为错误;deadline 到点由循环上浮)。
            Err(_) => return CallbackOutcome::Noise,
        }
    }
    let request = String::from_utf8_lossy(&buffer);
    let Some(request_line) = request.lines().next() else {
        return CallbackOutcome::Noise;
    };
    let mut parts = request_line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    if !method.eq_ignore_ascii_case("GET") {
        return CallbackOutcome::Noise;
    }
    let Some((_, query)) = target.split_once('?') else {
        // 无 query = 授权结论未携带(favicon/预连接等浏览器噪音)。
        return CallbackOutcome::Noise;
    };
    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    let mut provider_error: Option<String> = None;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let (Some(name), Some(value)) = (
            percent_decode_query_value(name),
            percent_decode_query_value(value),
        ) else {
            // 解码失败 = 无法信任的输入,fail closed。
            return CallbackOutcome::StateMismatch;
        };
        match name.as_str() {
            "code" => code = Some(value),
            "state" => state = Some(value),
            "error" => provider_error = Some(value),
            _ => {}
        }
    }
    // CSRF 防线(卡内 ①):state 缺失或不符 → 整次授权拒绝。
    if state.as_deref() != Some(expected_state) {
        return CallbackOutcome::StateMismatch;
    }
    if let Some(provider_error) = provider_error {
        // provider error 码经 expected_state 脱敏 + 截断入载荷(卡内 ⑦)。
        let detail = redact(&provider_error, expected_state);
        return CallbackOutcome::Denied(detail.chars().take(100).collect());
    }
    match code {
        Some(code) if !code.is_empty() => CallbackOutcome::Code(code),
        _ => CallbackOutcome::Noise,
    }
}

/// 写最小 HTML 回执页(best-effort:连接可能已被对端关闭,写失败静默——
/// 页面非授权结论通路,结论由返回值传递)。
fn write_html_page(stream: &mut TcpStream, status: u16, reason: &str, body: &str) {
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

/// 回执页面文案(静态英文,零 query 回显——GUI 资源化前的固定英文收尾页)。
const CALLBACK_SUCCESS_HTML: &str = "<html><body><p>Authorization complete. You can close this window and return to Partiverse.</p></body></html>";
const CALLBACK_STATE_HTML: &str = "<html><body><p>Authorization failed: invalid state parameter. Close this window and restart the authorization in Partiverse.</p></body></html>";
const CALLBACK_DENIED_HTML: &str = "<html><body><p>Authorization was denied or failed. Close this window and restart the authorization in Partiverse.</p></body></html>";
const CALLBACK_NOISE_HTML: &str = "<html><body><p>Bad Request</p></body></html>";

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试替身:模拟浏览器对回调服务器发一次 GET(返回整页响应文本)。
    fn fake_browser_get(uri: &str, target: &str) -> String {
        let authority = uri
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or_default();
        let mut stream = TcpStream::connect(authority).expect("connect callback server");
        let request =
            format!("GET {target} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).expect("write request");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");
        response
    }

    #[test]
    fn state_and_authorize_url_follow_provider_contract() {
        // 卡内 ①②④(baidu.md:21,26):state=32 位小写 hex 且非确定;URL 逐字符
        // 锚定授权契约;空 state/空 code 本地拒绝;Debug 全脱敏;缺省凭据=稳定
        // 文案 key 引导错误。
        let (first, second) = (
            generate_state().expect("state"),
            generate_state().expect("state"),
        );
        assert_eq!(first.len(), 32);
        assert!(
            first
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_ne!(first, second);
        assert_eq!(percent_encode_query_value("a b+c"), "a%20b%2Bc");
        let credentials =
            ClientCredentials::new("public-client-id", "s3cr3t-value").expect("credentials");
        assert!(format!("{credentials:?}").contains("public-client-id"));
        assert!(!format!("{credentials:?}").contains("s3cr3t-value"));
        let flow = BaiduOAuthFlow::new(credentials);
        let url = flow
            .authorize_url("0123456789abcdef0123456789abcdef")
            .expect("url");
        assert_eq!(
            url,
            "http://openapi.baidu.com/oauth/2.0/authorize?response_type=code\
             &client_id=public-client-id&redirect_uri=oob&scope=basic,netdisk\
             &state=0123456789abcdef0123456789abcdef"
        );
        assert!(matches!(
            kind_of(&flow.authorize_url("").expect_err("empty state")),
            Some(OAuthErrorKind::Invalid(_))
        ));
        assert!(matches!(
            kind_of(&flow.exchange_code("", "s").expect_err("empty code")),
            Some(OAuthErrorKind::Invalid(_))
        ));
        let token = TokenSet {
            access_token: "at-value".into(),
            refresh_token: "rt-value".into(),
            expires_in: 2_592_000,
        };
        let debug = format!("{token:?}");
        assert!(!debug.contains("at-value") && !debug.contains("rt-value"));
        assert!(debug.contains("2592000"));
        let err = ClientCredentials::new("  ", "").expect_err("defaulted creds rejected");
        assert_eq!(err.severity, Severity::Fatal);
        assert!(matches!(
            kind_of(&err),
            Some(OAuthErrorKind::ClientCredentialsMissing(key))
                if *key == CLIENT_CREDENTIALS_GUIDANCE_KEY
        ));
    }

    #[test]
    fn callback_server_state_gate_delivery_and_timeout() {
        // 卡内 ①⑥:state 精确匹配防 CSRF——交付(%XX 解码)/不符拒绝(fail
        // closed)/provider 拒绝三裁决;无连接到期 → Retryable Timeout(超时
        // 上浮);回执页零 query 回显(卡内 ⑦)。
        let server = LocalCallbackServer::bind().expect("bind");
        assert!(server.redirect_uri().starts_with("http://127.0.0.1:"));
        let uri = server.redirect_uri();
        let browser = std::thread::spawn(move || {
            fake_browser_get(&uri, "/callback?code=cb%2Dcode%2D9&state=state%2Dabc")
        });
        assert_eq!(
            server
                .wait_for_code("state-abc", Duration::from_secs(5))
                .expect("code"),
            "cb-code-9"
        );
        let page = browser.join().expect("browser thread");
        assert!(page.contains("200 OK") && page.contains("Authorization complete"));
        assert!(!page.contains("cb-code-9"));

        let server = LocalCallbackServer::bind().expect("bind");
        let uri = server.redirect_uri();
        let browser =
            std::thread::spawn(move || fake_browser_get(&uri, "/callback?code=x&state=tampered"));
        let err = server
            .wait_for_code("state-abc", Duration::from_secs(5))
            .expect_err("mismatch rejected");
        assert_eq!(err.severity, Severity::Fatal);
        assert!(matches!(kind_of(&err), Some(OAuthErrorKind::StateMismatch)));
        let page = browser.join().expect("browser thread");
        assert!(page.contains("400 Bad Request") && page.contains("invalid state"));

        let server = LocalCallbackServer::bind().expect("bind");
        let uri = server.redirect_uri();
        let browser = std::thread::spawn(move || {
            fake_browser_get(&uri, "/callback?error=access_denied&state=state-abc")
        });
        let err = server
            .wait_for_code("state-abc", Duration::from_secs(5))
            .expect_err("denial surfaced");
        let reason = match kind_of(&err) {
            Some(OAuthErrorKind::ProviderDenied(reason)) => reason.as_str(),
            _ => "",
        };
        assert_eq!((err.severity, reason), (Severity::Fatal, "access_denied"));
        assert!(
            browser
                .join()
                .expect("browser thread")
                .contains("denied or failed")
        );

        let server = LocalCallbackServer::bind().expect("bind");
        let err = server
            .wait_for_code("state-abc", Duration::from_millis(120))
            .expect_err("timeout surfaces");
        assert_eq!(err.severity, Severity::Retryable);
        assert!(matches!(kind_of(&err), Some(OAuthErrorKind::Timeout)));
    }
}
