//! M1-WP04-T02 集成测试(离线,卡内 ⑥⑦):本地 fixture HTTP 服务器
//! (`FakeTokenEndpoint`,mock 仅测试且命名 Fake*)扮演百度 token 端点——换码
//! 全链(URL 契约 → GET 五参数 → 解析 → sink 下沉)、非 2xx severity 映射、
//! 超时上浮、零明文(code/secret/token 不进错误/Debug);端点门(https 强制);
//! **真实网络零测试**,全部流量只到达 127.0.0.1 回环 fixture。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use partiverse_core::error::{PartisyError, Severity};
use partiverse_core::oauth::{
    BaiduOAuthFlow, ClientCredentials, OAuthErrorKind, OAuthTokenSink, TokenSet, kind_of,
};

/// 密值常量(仅测试进程内存;零明文断言的「不出现」基准)。
const FAKE_CODE: &str = "fake-code-123";
const FAKE_SECRET: &str = "fake-client-secret-value";
const FAKE_ACCESS: &str = "fake-access-token-value";
const FAKE_REFRESH: &str = "fake-refresh-token-value";
const FAKE_STATE: &str = "fake-state-0123456789abcdef";
/// 官方 30 天(baidu.md:24)。
const EXPIRES_30D: u64 = 2_592_000;

/// 本地 fixture token 端点(std TcpListener):记录请求行供契约断言,按 `delay`
/// 延迟后回固定状态行+JSON 体。
struct FakeTokenEndpoint {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl FakeTokenEndpoint {
    /// 启动 fixture(回环随机端口;零真实网络)。
    fn spawn(status_line: &'static str, body: String, delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake endpoint");
        let addr = listener.local_addr().expect("fake endpoint addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (log, shutdown) = (requests.clone(), Arc::new(AtomicBool::new(false)));
        let worker_shutdown = shutdown.clone();
        let worker = std::thread::spawn(move || {
            let _ = listener.set_nonblocking(true);
            // nonblocking 轮询 accept:shutdown 置位即退出,Drop 必达(「零
            // 请求」用例若用阻塞 accept 会在 Drop 时永久悬挂)。
            while !worker_shutdown.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                        let mut buffer = [0u8; 8192];
                        let n = stream.read(&mut buffer).unwrap_or(0);
                        if let Some(line) = String::from_utf8_lossy(&buffer[..n]).lines().next() {
                            log.lock().expect("request log").push(line.to_owned());
                        }
                        std::thread::sleep(delay);
                        let response = format!(
                            "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        FakeTokenEndpoint {
            addr,
            requests,
            shutdown,
            worker: Some(worker),
        }
    }

    /// fixture 端点 URL(回环 http,走 `with_token_endpoint` 测试注入缝)。
    fn url(&self) -> String {
        format!("http://{}/oauth/2.0/token", self.addr)
    }

    /// 首个请求行(GET + query;契约断言用)。
    fn first_request_line(&self) -> String {
        self.requests
            .lock()
            .expect("request log")
            .first()
            .cloned()
            .unwrap_or_default()
    }
}

impl Drop for FakeTokenEndpoint {
    fn drop(&mut self) {
        // 置位 shutdown → worker 轮询循环必达退出 → join 有界(零悬挂)。
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// 测试替身 sink(命名 Fake*,卡内边界):内存保存下沉 token。
#[derive(Debug, Default)]
struct FakeTokenSink {
    stored: Mutex<Option<TokenSet>>,
}

impl OAuthTokenSink for FakeTokenSink {
    fn store_token(&self, token: &TokenSet) -> Result<(), PartisyError> {
        *self.stored.lock().expect("fake sink") = Some(token.clone());
        Ok(())
    }
}

/// 构造注入 fixture 端点的流(公共步;端点门在 `with_token_endpoint`)。
fn flow_against(endpoint_url: &str) -> BaiduOAuthFlow {
    let credentials =
        ClientCredentials::new("fake-client-id", FAKE_SECRET).expect("credentials valid");
    BaiduOAuthFlow::new(credentials)
        .with_token_endpoint(endpoint_url)
        .expect("loopback fixture endpoint accepted")
}

/// 官方形状的成功响应体。
fn token_body() -> String {
    format!(
        r#"{{"access_token":"{FAKE_ACCESS}","refresh_token":"{FAKE_REFRESH}","expires_in":{EXPIRES_30D}}}"#
    )
}

#[test]
fn exchange_full_chain_offline_reaches_local_sink() {
    // 卡内 ⑥「换码全链」:authorize URL 契约(baidu.md:21)→ GET 换码契约
    // (baidu.md:23,grant_type/code/client_id/client_secret/redirect_uri)→
    // TokenSet 解析(baidu.md:24-25)→ sink 下沉(核心零持久化)。
    let fake = FakeTokenEndpoint::spawn("200 OK", token_body(), Duration::ZERO);
    let flow = flow_against(&fake.url());
    let url = flow.authorize_url(FAKE_STATE).expect("authorize url");
    assert!(url.starts_with(
        "http://openapi.baidu.com/oauth/2.0/authorize?response_type=code&client_id=fake-client-id"
    ));
    assert!(url.contains("&redirect_uri=oob&scope=basic,netdisk&state="));
    let token = flow.exchange_code(FAKE_CODE, FAKE_STATE).expect("exchange");
    assert_eq!(
        (
            token.access_token.as_str(),
            token.refresh_token.as_str(),
            token.expires_in
        ),
        (FAKE_ACCESS, FAKE_REFRESH, EXPIRES_30D)
    );
    let sink = FakeTokenSink::default();
    sink.store_token(&token).expect("sink store");
    assert_eq!(
        sink.stored.lock().expect("fake sink").as_ref(),
        Some(&token)
    );
    let line = fake.first_request_line();
    assert!(
        line.starts_with("GET /oauth/2.0/token?"),
        "token 端点 GET: {line}"
    );
    assert!(line.contains("grant_type=authorization_code"));
    assert!(line.contains(&format!("code={FAKE_CODE}")));
    assert!(line.contains("client_id=fake-client-id"));
    assert!(line.contains(&format!("client_secret={FAKE_SECRET}")));
    assert!(line.contains("redirect_uri=oob"));
    assert!(
        !line.contains("state="),
        "state 不入 token 请求(baidu.md:23)"
    );
}

#[test]
fn provider_error_on_2xx_is_rejected_without_leaking_secrets() {
    // 卡内 ⑦:2xx error 体(error_description 回显 code/secret/token)→
    // 结构化拒绝,错误 Display/Debug 零明文(载荷经 redact 过滤)。
    let body = format!(
        r#"{{"error":"invalid_grant","error_description":"code {FAKE_CODE} secret {FAKE_SECRET} token {FAKE_ACCESS} rejected"}}"#
    );
    let fake = FakeTokenEndpoint::spawn("200 OK", body, Duration::ZERO);
    let err = flow_against(&fake.url())
        .exchange_code(FAKE_CODE, FAKE_STATE)
        .expect_err("2xx error body must reject");
    assert_eq!(err.severity, Severity::Fatal);
    assert!(matches!(
        kind_of(&err),
        Some(OAuthErrorKind::ExchangeResponseInvalid(_))
    ));
    for secret in [FAKE_CODE, FAKE_SECRET, FAKE_ACCESS, FAKE_REFRESH] {
        assert!(!format!("{err}").contains(secret), "display 泄漏 {secret}");
        assert!(!format!("{err:?}").contains(secret), "debug 泄漏 {secret}");
    }
    assert!(
        format!("{err}").contains("invalid_grant"),
        "脱敏后的 provider 错误码保留"
    );
}

#[test]
fn http_status_mapping_and_timeout_surfaces() {
    // 非 2xx:状态码入错误、响应体不入(体可能回显参数);severity 表驱动
    // (400=Fatal,429=Retryable)。拖延应答的 fixture + 短超时 → Retryable
    // Timeout 显式上浮(卡内 ⑥「超时上浮」,零吞错零挂死)。
    let fatal = FakeTokenEndpoint::spawn("400 Bad Request", "{}".into(), Duration::ZERO);
    let err = flow_against(&fatal.url())
        .exchange_code(FAKE_CODE, FAKE_STATE)
        .expect_err("400 rejects");
    assert!(matches!(
        kind_of(&err),
        Some(OAuthErrorKind::ExchangeHttpStatus(400))
    ));
    assert_eq!(err.severity, Severity::Fatal);
    assert!(!format!("{err}").contains(FAKE_SECRET));

    let throttled = FakeTokenEndpoint::spawn("429 Too Many Requests", "{}".into(), Duration::ZERO);
    let err = flow_against(&throttled.url())
        .exchange_code(FAKE_CODE, FAKE_STATE)
        .expect_err("429 surfaces");
    assert!(matches!(
        kind_of(&err),
        Some(OAuthErrorKind::ExchangeHttpStatus(429))
    ));
    assert_eq!(err.severity, Severity::Retryable);

    let slow = FakeTokenEndpoint::spawn("200 OK", token_body(), Duration::from_millis(1500));
    let flow = flow_against(&slow.url()).with_exchange_timeout(Duration::from_millis(200));
    let err = flow
        .exchange_code(FAKE_CODE, FAKE_STATE)
        .expect_err("timeout surfaces");
    assert_eq!(err.severity, Severity::Retryable);
    assert!(matches!(kind_of(&err), Some(OAuthErrorKind::Timeout)));
}

#[test]
fn non_https_remote_endpoint_is_rejected_before_any_request() {
    // ADR-0004:证书校验不得关闭;非回环 http 端点在发起请求前本地拒绝(Fatal)。
    let credentials =
        ClientCredentials::new("fake-client-id", FAKE_SECRET).expect("credentials valid");
    let result =
        BaiduOAuthFlow::new(credentials).with_token_endpoint("http://example.test/oauth/2.0/token");
    let err = match result {
        Ok(_) => panic!("non-loopback http endpoint must be rejected"),
        Err(err) => err,
    };
    assert_eq!(err.severity, Severity::Fatal);
    assert!(matches!(kind_of(&err), Some(OAuthErrorKind::Invalid(_))));
}
