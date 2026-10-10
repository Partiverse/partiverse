//! 123 云盘官方 WebDAV 接入流(M1-WP04-T03;provider 事实 =
//! docs/providers/123pan.md §7,2026-10-08 核实)。
//!
//! 纯函数面:端点 + 账号 + 应用专用密码 → [`RemoteCreate`](webdav vendor
//! remote 描述),交 [`crate::schema::create_remote`] 注入;轻探针(连通性
//! 体检)属后续卡,本卡只建 remote + `config/get` 回读断言(卡内 ③)。
//!
//! # Provider 事实(§7;本模块代码零端点/域名硬编码,卡内禁止行为)
//! - 定位与门槛:官方公开 WebDAV 功能,**VIP 及以上**(免费用户无;会员
//!   专项流量额度官方未公布,速度波动)。
//! - 开启路径:网页端「第三方挂载」→「WebDAV 授权管理」→ 添加应用 →
//!   **生成应用专用密码(只显示一次,可随时撤销;非账号登录密码,按应用
//!   隔离)**——文案 key [`PAN123_VIP_NOTICE_KEY`]/
//!   [`PAN123_ENDPOINT_GUIDANCE_KEY`]/[`PAN123_APP_PASSWORD_GUIDANCE_KEY`]
//!   (卡内 ④;英文正文与 key 同源交付,资源化与 UI 接线属 WP05)。
//! - 端点:按账号个性化——**必须用户从控制台复制粘贴**,本模块只做
//!   https 协议校验(官方端点形态均为 https),零域名/端点常量。
//! - 鉴权:HTTP Basic,账号 = 注册手机号/邮箱,密码 = 应用专用密码。
//! - vendor 归类:1.75.1 实机 `config/providers` webdav.vendor 的 8 个
//!   Examples 枚举(fastmail/nextcloud/owncloud/infinitescale/sharepoint/
//!   sharepoint-ntlm/rclone/other)无 123 条目 → 按通用 WebDAV 服务取
//!   "other"。
//! - 凭据面:应用专用密码全程脱敏(三覆盖面:载体
//!   [`RemoteCreate`] 手写 Debug 零值、[`create_remote`] 错误载荷 redact
//!   兜底、本模块错误载荷零用户粘贴值;单测逐面断言)。

use std::fmt;

use crate::error::{PartisyError, Severity};
use crate::schema::RemoteCreate;

/// 123 VIP 权益注明文案 key(卡内 ④;§7 门槛与额度事实)。
pub const PAN123_VIP_NOTICE_KEY: &str = "connection.123pan.vip-requirement";
/// 上行英文正文(GUI 唯一语言 = English,资源化前直用)。
pub const PAN123_VIP_NOTICE_EN: &str = "123 Cloud WebDAV requires a paid VIP or higher plan. Transfer speed and monthly traffic quotas for WebDAV are set by 123 and may change at any time.";

/// 端点复制引导文案 key(卡内 ④;§7 端点按账号个性化、控制台复制)。
pub const PAN123_ENDPOINT_GUIDANCE_KEY: &str = "connection.123pan.endpoint.copy";
/// 上行英文正文(零域名出现——端点只能由用户从控制台粘贴)。
pub const PAN123_ENDPOINT_GUIDANCE_EN: &str = "The WebDAV endpoint is specific to your account. Sign in to the 123pan website, open Third-party Mounting > WebDAV Authorization Management, and paste the endpoint URL shown there. Partiverse does not preset the endpoint.";

/// 应用专用密码生成引导文案 key(卡内 ④;§7 生成与一次性显示事实)。
pub const PAN123_APP_PASSWORD_GUIDANCE_KEY: &str = "connection.123pan.app-password.generate";
/// 上行英文正文(强调一次性显示、可撤销、非登录密码)。
pub const PAN123_APP_PASSWORD_GUIDANCE_EN: &str = "On the same WebDAV authorization page, add an application to generate an app-specific password. It is shown only once, can be revoked at any time, and is not your account login password.";

/// vendor 固定取值(见模块头「vendor 归类」;非端点,系 schema 枚举归类)。
const PAN123_WEBDAV_VENDOR: &str = "other";

/// 结构化错误类别(载荷只含结构性描述,零端点/账号/密码回显——卡内 ⑦)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pan123ErrorKind {
    /// 输入不合法(载荷 = 结构性描述,Fatal;用户修正后可重试)。
    Invalid(String),
}

impl fmt::Display for Pan123ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Pan123ErrorKind::Invalid(detail) => write!(f, "invalid 123pan webdav input: {detail}"),
        }
    }
}

impl std::error::Error for Pan123ErrorKind {}

impl Pan123ErrorKind {
    /// Fatal 构造(输入校验失败无瞬态语义,credential_store 同款)。
    fn fatal(self) -> PartisyError {
        PartisyError {
            severity: Severity::Fatal,
            source: Some(Box::new(self)),
        }
    }
}

/// 从 [`PartisyError`] 还原结构化类别(非本模块来源的错误 → None)。
#[must_use]
pub fn kind_of(err: &PartisyError) -> Option<&Pan123ErrorKind> {
    err.source
        .as_ref()
        .and_then(|s| s.downcast_ref::<Pan123ErrorKind>())
}

/// 构造 123 官方 WebDAV 形态的 remote 描述(卡内 ③纯函数;零 IO,离线可测)。
///
/// 参数纪律(§7,模块头):端点 = 用户从控制台复制的完整 https URL(仅协议
/// 校验,零域名硬编码);账号 = 注册手机号/邮箱(收尾去空白,粘贴容错);
/// 应用专用密码 = 逐字符保真(不 trim,密码语义下空白合法)。
///
/// # Errors
/// 名称为空 / 端点非 https / 账号或应用专用密码为空 →
/// [`Pan123ErrorKind::Invalid`](Fatal,载荷零用户粘贴值)。
pub fn webdav_remote(
    name: &str,
    endpoint: &str,
    account: &str,
    app_password: &str,
) -> Result<RemoteCreate, PartisyError> {
    let name = name.trim();
    let endpoint = endpoint.trim();
    let account = account.trim();
    if name.is_empty() {
        return Err(Pan123ErrorKind::Invalid("remote name must not be empty".into()).fatal());
    }
    // 官方端点均为 https(§7 端点形态;域名不校验——按账号个性化且零硬编码)。
    if !endpoint.starts_with("https://") {
        return Err(Pan123ErrorKind::Invalid(
            "endpoint must be an https url copied from the 123pan web console".into(),
        )
        .fatal());
    }
    if account.is_empty() {
        return Err(
            Pan123ErrorKind::Invalid("account (phone or email) must not be empty".into()).fatal(),
        );
    }
    if app_password.is_empty() {
        return Err(Pan123ErrorKind::Invalid(
            "app-specific password must not be empty (generate it in WebDAV Authorization Management)"
                .into(),
        )
        .fatal());
    }
    Ok(RemoteCreate::new(name, "webdav")
        .with_parameter("url", endpoint.to_owned())
        .with_parameter("vendor", PAN123_WEBDAV_VENDOR.to_owned())
        .with_parameter("user", account.to_owned())
        .with_parameter("pass", app_password.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const ENDPOINT: &str = "https://webdav.example.invalid/webdav";
    const ACCOUNT: &str = "13800000000";
    const APP_PASS: &str = "wp04t03-app-pass-9f3a";

    #[test]
    fn webdav_remote_builds_card_shape_with_exact_parameters() {
        // 卡内 ③:端点+账号+应用专用密码 → webdav 形态四参数(url/vendor/
        // user/pass);名称收尾去空白;密码逐字符保真(空白不裁)。
        let remote = webdav_remote("  pv123  ", ENDPOINT, ACCOUNT, APP_PASS).expect("builds");
        assert_eq!(remote.name, "pv123");
        assert_eq!(remote.remote_type, "webdav");
        assert_eq!(remote.parameters.get("url"), Some(&Value::from(ENDPOINT)));
        assert_eq!(remote.parameters.get("vendor"), Some(&Value::from("other")));
        assert_eq!(remote.parameters.get("user"), Some(&Value::from(ACCOUNT)));
        assert_eq!(remote.parameters.get("pass"), Some(&Value::from(APP_PASS)));
        assert_eq!(remote.parameters.len(), 4);
    }

    #[test]
    fn app_password_is_redacted_on_every_surface() {
        // 卡内 ③:应用专用密码全程脱敏——Debug 零值、本模块错误载荷零用户
        // 粘贴值(结构性描述含字段语义但不含值)。
        let remote = webdav_remote("pv123", ENDPOINT, ACCOUNT, APP_PASS).expect("builds");
        let debug = format!("{remote:?}");
        assert!(!debug.contains(APP_PASS) && !debug.contains(ACCOUNT));
        assert!(!debug.contains(ENDPOINT));
        for (endpoint, account, pass) in [
            ("http://webdav.example.invalid/webdav", ACCOUNT, APP_PASS),
            (ENDPOINT, "", APP_PASS),
            (ENDPOINT, ACCOUNT, ""),
        ] {
            let err =
                webdav_remote("pv123", endpoint, account, pass).expect_err("invalid rejected");
            assert_eq!(err.severity, Severity::Fatal);
            assert!(matches!(kind_of(&err), Some(Pan123ErrorKind::Invalid(_))));
            let text = err.to_string();
            for secret in [endpoint, ACCOUNT, APP_PASS, ENDPOINT] {
                assert!(!text.contains(secret), "leak in error text: {text}");
            }
        }
        // 空名称同面拒绝。
        let err = webdav_remote("  ", ENDPOINT, ACCOUNT, APP_PASS).expect_err("empty name");
        assert!(matches!(kind_of(&err), Some(Pan123ErrorKind::Invalid(_))));
    }

    #[test]
    fn guidance_keys_carry_english_copy_without_pinning_endpoints() {
        // 卡内 ④:三文案 key/英文正文成对交付;正文零域名/端点形态出现
        // (端点域名零硬编码,控制台复制引导语义);key 命名空间统一。
        for (key, text) in [
            (PAN123_VIP_NOTICE_KEY, PAN123_VIP_NOTICE_EN),
            (PAN123_ENDPOINT_GUIDANCE_KEY, PAN123_ENDPOINT_GUIDANCE_EN),
            (
                PAN123_APP_PASSWORD_GUIDANCE_KEY,
                PAN123_APP_PASSWORD_GUIDANCE_EN,
            ),
        ] {
            assert!(
                key.starts_with("connection.123pan."),
                "key namespace: {key}"
            );
            assert!(text.is_ascii(), "english-only copy: {key}");
            assert!(!text.contains(".cn"), "no pinned domain in copy: {key}");
            assert!(
                !text.contains("webdav-"),
                "no endpoint shape in copy: {key}"
            );
        }
        assert!(PAN123_VIP_NOTICE_EN.contains("VIP"));
        assert!(PAN123_ENDPOINT_GUIDANCE_EN.contains("paste the endpoint URL"));
        assert!(PAN123_APP_PASSWORD_GUIDANCE_EN.contains("shown only once"));
        assert!(PAN123_APP_PASSWORD_GUIDANCE_EN.contains("not your account login password"));
    }
}
