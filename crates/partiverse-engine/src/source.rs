//! 下载源抽象(M1-WP02-T01):传输与校验解耦(D31:镜像只换传输不换校验)。
//!
//! [`HttpSource`] 为生产源:ureq 同步客户端,默认 rustls+ring,证书校验强制、
//! 任何代码路径无关闭开关;离线单测用 installer.rs 内 Fake* 替身,禁止真实网络。

use std::io;
use std::time::Duration;

use crate::error::{SourceFailureKind, error_chain};

/// 单次下载全局超时(覆盖连接+响应+响应体读取;官方 zip 约 31-35 MiB,留足余量)。
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// 包源抽象:把完整 URL 指向的构件取回调用方提供的 writer。
///
/// 目标文件生命周期与 IO 错误语义归调用方(installer)所有:本地落盘类 IO
/// 错误属环境问题,按 `classify_io` 分级上浮,不冒充源故障。
pub trait PackageSource {
    /// 取回构件写入 `writer`;失败返回结构化单源原因(由 installer 聚合上浮)。
    fn fetch_into(&self, url: &str, writer: &mut dyn io::Write) -> Result<(), SourceFailureKind>;
}

/// 生产 HTTP 源(自动读取 ALL_PROXY/HTTPS_PROXY/HTTP_PROXY 代理环境变量)。
#[derive(Debug)]
pub struct HttpSource {
    agent: ureq::Agent,
}

impl HttpSource {
    /// 新建源客户端(全局超时见 [`DOWNLOAD_TIMEOUT`])。
    #[must_use]
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(DOWNLOAD_TIMEOUT))
            .build();
        HttpSource {
            agent: config.into(),
        }
    }
}

impl Default for HttpSource {
    fn default() -> Self {
        Self::new()
    }
}

impl PackageSource for HttpSource {
    fn fetch_into(&self, url: &str, writer: &mut dyn io::Write) -> Result<(), SourceFailureKind> {
        // 纵深防御:除 manifest 解析期 HTTPS 强制外,取回前再校验一次实际 URL。
        if !url.starts_with("https://") {
            return Err(SourceFailureKind::Transport(format!(
                "non-https url rejected: {url}"
            )));
        }
        // 默认 http_status_as_error 开启:非 2xx 直接转为 Error::StatusCode(code)。
        let response = self.agent.get(url).call().map_err(|err| match err {
            ureq::Error::StatusCode(code) => SourceFailureKind::HttpStatus(code),
            other => SourceFailureKind::Transport(error_chain(&other)),
        })?;
        // 流式落盘:响应体边下边写,截断/中断由后续 sha256 校验兜底,不会被误装。
        let mut reader = response.into_body().into_reader();
        io::copy(&mut reader, writer)
            .map_err(|err| SourceFailureKind::Transport(format!("body copy failed: {err}")))?;
        writer
            .flush()
            .map_err(|err| SourceFailureKind::Transport(format!("dest flush failed: {err}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 非 https 地址必须在任何连接尝试前被拒(不发起网络请求即可测)。
    #[test]
    fn non_https_url_is_rejected_before_any_connection() {
        let source = HttpSource::new();
        let mut sink: Vec<u8> = Vec::new();
        let err = source
            .fetch_into("http://example.test/rclone.zip", &mut sink)
            .expect_err("http url must be rejected");
        assert!(matches!(err, SourceFailureKind::Transport(_)));
    }
}
