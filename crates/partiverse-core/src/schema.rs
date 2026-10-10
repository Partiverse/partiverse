//! rc `config/providers` schema 表单描述模型 + `config/create` 提交通道
//! (M1-WP04-T03)。
//!
//! 构成(卡内 DoD):①模型 = [`ProviderForm`]/[`FieldDescription`](serde
//! 结构,架构 §5 映射清单:Type/Required/IsPassword/Advanced/Exclusive +
//! 默认值;WP05 向导直接渲染该模型,模型零 UI 概念——React 实件属 WP05);
//! ②拉取/注入统一走 T01 白名单接缝 [`RcDispatch`](`config/providers`/
//! `config/create` 均在白名单,任意字符串经引擎 `RcMethod` 编译期枚举二次
//! 收口);③零明文面:[`RemoteCreate`] 手写 [`fmt::Debug`] 只出参数键名零值,
//! [`create_remote`] 失败路径对全部参数值做 [`redact`] 兜底后才进错误载荷。
//!
//! # rc 形状实测锚定(2026-10-10,宿主件 = 引擎 manifest 同版 rclone v1.75.1)
//! - `config/providers` → `{"providers":[{Name,Description,Prefix,Options,
//!   CommandHelp,Aliases,Hide,MetadataInfo,Overview},…]}`;option 侧 =
//!   `{Name,FieldName,Help,Default,Value,Hide,Required,IsPassword,NoPrefix,
//!   Advanced,Exclusive,Sensitive,DefaultStr,ValueStr,Type[,Examples]}`。
//!   本模块**只映射卡内清单七属性**,其余字段(含上游日后新增)宽容忽略。
//! - `Type` 全集(69 个 provider 实测):string/bool/int/float64/Duration/
//!   SizeSuffix/Time/Tristate/Bits/CommaSepList/SpaceSepList/Encoding/
//!   stringArray/mtime|atime|btime|ctime——模型按原样字符串透传,不枚举
//!   (枚举 = 凭记忆固化上游类型面,上游加型即破)。
//! - `Default` 为类型化 JSON(Tristate=对象、列表型=数组、Time=字符串)。
//! - `config/create` 入参 `{"name","type","parameters"[,"obscure"]}`:同
//!   名重复创建 = 静默覆盖(实测);密码字段由 rclone obscure 后落加密
//!   config(显式传 `obscure:true`,1.75.1 默认即 obscure,显式化防上游
//!   默认漂移);`config/get` `{"name"}` 回读扁平 remote 配置,密码为
//!   obscured 形态而非明文。
//! - 错误回包结构:`{"error", "input", "path", "status"}`,`input` 会回显
//!   全部入参(含明文密码)。引擎 [`RcClient`] 只取 `error` 字段,但值级
//!   防御在本通道强制([`redact`] 兜底),保证 rc 侧措辞变化也不破零明文。

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::credential_store::redact;
use crate::error::{PartisyError, Severity};
use crate::jobs::RcDispatch;

/// 结构化错误类别(载荷只含结构性描述,零用户粘贴值——卡内 ⑦)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaErrorKind {
    /// `config/providers` 载荷不符合实测锚定形状(协议漂移,Fatal)。
    ResponseInvalid(String),
    /// 请求的 provider 不在 schema 列表中(载荷=provider 名,Fatal)。
    ProviderNotFound(String),
}

impl fmt::Display for SchemaErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SchemaErrorKind::ResponseInvalid(detail) => {
                write!(f, "config/providers payload invalid: {detail}")
            }
            SchemaErrorKind::ProviderNotFound(name) => {
                write!(f, "provider not found in config/providers: {name}")
            }
        }
    }
}

impl std::error::Error for SchemaErrorKind {}

impl SchemaErrorKind {
    /// Fatal 构造(schema 形状/查找失败均无瞬态语义,credential_store 同款)。
    fn fatal(self) -> PartisyError {
        PartisyError {
            severity: Severity::Fatal,
            source: Some(Box::new(self)),
        }
    }
}

/// 从 [`PartisyError`] 还原结构化类别(非本模块来源的错误 → None)。
#[must_use]
pub fn kind_of(err: &PartisyError) -> Option<&SchemaErrorKind> {
    err.source
        .as_ref()
        .and_then(|s| s.downcast_ref::<SchemaErrorKind>())
}

/// 单个连接表单字段的描述模型(卡内 ①七属性,serde 结构直映 rc 输出;
/// 未知字段宽容忽略——rclone 跨版本增字段不破模型)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldDescription {
    /// 字段名(rc option `Name`,提交 `config/create` 时的参数键)。
    #[serde(rename = "Name")]
    pub name: String,
    /// 值类型原样字符串(实测全集见模块头;向导按此分支渲染控件)。
    #[serde(rename = "Type")]
    pub field_type: String,
    /// 必填标记(rc `Required`)。
    #[serde(rename = "Required")]
    pub required: bool,
    /// 密码字段标记(rc `IsPassword`;UI 与日志全程脱敏)。
    #[serde(rename = "IsPassword")]
    pub is_password: bool,
    /// 高级选项标记(rc `Advanced`;向导折叠区渲染)。
    #[serde(rename = "Advanced")]
    pub advanced: bool,
    /// 枚举互斥标记(rc `Exclusive`;true 时取值限于候选集)。
    #[serde(rename = "Exclusive")]
    pub exclusive: bool,
    /// 默认值(类型化 JSON,缺省为 null)。
    #[serde(rename = "Default", default)]
    pub default: Value,
}

/// 单个 provider 的连接表单 schema(卡内 ①「表单描述模型」的 provider 层)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderForm {
    /// provider 名(rclone backend 名,如 "webdav";`config/create` 的 type)。
    #[serde(rename = "Name")]
    pub name: String,
    /// 描述文本(rc `Description`;向导标题/说明渲染用)。
    #[serde(rename = "Description")]
    pub description: String,
    /// 字段描述列表(rc `Options`,保序)。
    #[serde(rename = "Options", default)]
    pub fields: Vec<FieldDescription>,
}

impl ProviderForm {
    /// 按字段名取字段描述(向导渲染查表面;唯一时命中首个)。
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&FieldDescription> {
        self.fields.iter().find(|field| field.name == name)
    }
}

/// rc 回复信封(实测顶层键 `providers`;缺失 = 协议漂移,结构化拒绝)。
#[derive(Deserialize)]
struct ProvidersReply {
    providers: Vec<ProviderForm>,
}

/// 解析 `config/providers` 成功载荷为表单模型(拉取与解析分离,单测可用
/// 真机 fixture 直接锚定形状)。
///
/// # Errors
/// 载荷缺 `providers` 数组或元素形状不符 → [`SchemaErrorKind::ResponseInvalid`]
/// (Fatal,零吞错)。
pub fn parse_provider_forms(reply: Value) -> Result<Vec<ProviderForm>, PartisyError> {
    let reply: ProvidersReply = serde_json::from_value(reply).map_err(|err| {
        SchemaErrorKind::ResponseInvalid(format!("config/providers payload: {err}")).fatal()
    })?;
    Ok(reply.providers)
}

/// 经 T01 白名单接缝拉取全部 provider 表单 schema(卡内 ①「真 rclone 集成」;
/// 请求入参为空对象,无凭据面,失败原样上浮)。
///
/// # Errors
/// rc 调用失败按引擎 severity 保真上浮;载荷形状漂移 →
/// [`SchemaErrorKind::ResponseInvalid`]。
pub fn provider_forms(dispatch: &impl RcDispatch) -> Result<Vec<ProviderForm>, PartisyError> {
    let reply = dispatch.call("config/providers", &serde_json::json!({}))?;
    parse_provider_forms(reply)
}

/// 在表单列表中按 provider 名查找(`form_for_provider` 的无 IO 面)。
#[must_use]
pub fn provider_form<'a>(forms: &'a [ProviderForm], name: &str) -> Option<&'a ProviderForm> {
    forms.iter().find(|form| form.name == name)
}

/// 拉取并定位单个 provider 的表单 schema(卡内 ①;不存在 → 结构化 Fatal)。
///
/// # Errors
/// 见 [`provider_forms`];未命中 → [`SchemaErrorKind::ProviderNotFound`]。
pub fn form_for_provider(
    dispatch: &impl RcDispatch,
    name: &str,
) -> Result<ProviderForm, PartisyError> {
    let forms = provider_forms(dispatch)?;
    provider_form(&forms, name)
        .cloned()
        .ok_or_else(|| SchemaErrorKind::ProviderNotFound(name.to_owned()).fatal())
}

/// 待创建 remote 的表单值载体(卡内 ②「表单值→config/create 注入」;
/// 由 [`form_for_provider`] 渲染产出的表单收集而来,生产构造面 = WP05 向导,
/// 本卡生产构造面 = [`crate::pan123::webdav_remote`])。
#[derive(Clone, PartialEq)]
pub struct RemoteCreate {
    /// remote 名(rclone config 内唯一)。
    pub name: String,
    /// provider/backend 名(即 [`ProviderForm::name`],如 "webdav")。
    pub remote_type: String,
    /// 表单值(字段名 → 类型化值;密码为用户明文,交 rclone obscure)。
    pub parameters: BTreeMap<String, Value>,
}

impl RemoteCreate {
    /// 构造空参数载体(参数经 [`Self::with_parameter`] 逐项注入)。
    #[must_use]
    pub fn new(name: impl Into<String>, remote_type: impl Into<String>) -> Self {
        RemoteCreate {
            name: name.into(),
            remote_type: remote_type.into(),
            parameters: BTreeMap::new(),
        }
    }

    /// 注入一个表单值(链式)。
    #[must_use]
    pub fn with_parameter(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.parameters.insert(name.into(), value.into());
        self
    }
}

/// 手写 Debug:只出参数键名,零值(卡内 ③密码全程脱敏——Debug 打印面
/// 不区分是否密码字段,统一不落任何表单值)。
impl fmt::Debug for RemoteCreate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteCreate")
            .field("name", &self.name)
            .field("remote_type", &self.remote_type)
            .field(
                "parameter_keys",
                &self.parameters.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// `config/create` 被拒的结构化载荷(卡内 ③:detail = 引擎错误文本经全部
/// 参数值 [`redact`] 兜底;severity 沿用引擎分级,零吞错)。
#[derive(Debug)]
pub struct CreateRejection {
    /// 被拒的 remote 名(非凭据,入载荷辅助定位)。
    pub remote: String,
    /// 引擎错误文本(参数值已置换为 `<redacted>`)。
    pub detail: String,
}

impl fmt::Display for CreateRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "config/create rejected for remote {:?}: {}",
            self.remote, self.detail
        )
    }
}

impl std::error::Error for CreateRejection {}

/// 从 [`PartisyError`] 还原 create 被拒载荷(非本通道来源的错误 → None)。
#[must_use]
pub fn rejection_of(err: &PartisyError) -> Option<&CreateRejection> {
    err.source
        .as_ref()
        .and_then(|s| s.downcast_ref::<CreateRejection>())
}

/// 提交通道(卡内 ②):表单值 → `config/create` 注入(白名单内,经 T01
/// RcDispatch;`obscure:true` 显式化——密码由 rclone obscure 后落加密
/// config,架构 §5「凭据单一真相源」)。失败 = 引擎 severity 保真 + 载荷
/// 经全部参数值 [`redact`] 兜底(实测 rc 错误回包 input 回显全部入参,值级
/// 防御不依赖上游措辞;零吞错:detail 保留非凭据根因)。
///
/// # Errors
/// rc 调用失败 → [`CreateRejection`](severity 沿引擎分级);成功载荷被忽略
/// (实测为 `{}`)。
pub fn create_remote(
    dispatch: &impl RcDispatch,
    remote: &RemoteCreate,
) -> Result<(), PartisyError> {
    let parameters: serde_json::Map<String, Value> = remote
        .parameters
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let params = serde_json::json!({
        "name": remote.name,
        "type": remote.remote_type,
        "parameters": Value::Object(parameters),
        "obscure": true,
    });
    dispatch
        .call("config/create", &params)
        .map(|_| ())
        .map_err(|err| {
            let mut detail = err.to_string();
            for value in remote.parameters.values().filter_map(Value::as_str) {
                detail = redact(&detail, value);
            }
            PartisyError::with_source(
                err.severity,
                Box::new(CreateRejection {
                    remote: remote.name.clone(),
                    detail,
                }),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Severity;
    use serde_json::json;
    use std::sync::Mutex;

    /// 真机 fixture(2026-10-10 实机 `config/providers` webdav 条目逐字节
    /// 摘录,前 6 个 option;禁止凭记忆编字段的锚定物)。只截断条目数量,
    /// 保留的字段全部原样。
    /// 真机 fixture(2026-10-10 实机 `config/providers` webdav 条目逐字段
    /// 摘录:仅截断 option 数量,保留条目的键/值/Help 原文逐字不变,空白为
    /// 排版压缩;禁止凭记忆编字段的锚定物)。
    const WEBDAV_FIXTURE: &str = r#"{"providers": [{
      "Name": "webdav", "Description": "WebDAV", "Prefix": "webdav",
      "Options": [
        {"Name": "url", "FieldName": "", "Help": "URL of http host to connect to.\n\nE.g. https://example.com.", "Default": "", "Value": null, "Hide": 0, "Required": true, "IsPassword": false, "NoPrefix": false, "Advanced": false, "Exclusive": false, "Sensitive": false, "DefaultStr": "", "ValueStr": "", "Type": "string"},
        {"Name": "vendor", "FieldName": "", "Help": "Name of the WebDAV site/service/software you are using.", "Default": "", "Value": null, "Examples": [{"Value": "fastmail", "Help": "Fastmail Files"}, {"Value": "nextcloud", "Help": "Nextcloud"}, {"Value": "owncloud", "Help": "Owncloud 10 PHP based WebDAV server"}, {"Value": "infinitescale", "Help": "ownCloud Infinite Scale"}, {"Value": "sharepoint", "Help": "Sharepoint Online, authenticated by Microsoft account"}, {"Value": "sharepoint-ntlm", "Help": "Sharepoint with NTLM authentication, usually self-hosted or on-premises"}, {"Value": "rclone", "Help": "rclone WebDAV server to serve a remote over HTTP via the WebDAV protocol"}, {"Value": "other", "Help": "Other site/service or software"}], "Hide": 0, "Required": false, "IsPassword": false, "NoPrefix": false, "Advanced": false, "Exclusive": false, "Sensitive": false, "DefaultStr": "", "ValueStr": "", "Type": "string"},
        {"Name": "user", "FieldName": "", "Help": "User name.\n\nIn case NTLM authentication is used, the username should be in the format 'Domain\\User'.", "Default": "", "Value": null, "Hide": 0, "Required": false, "IsPassword": false, "NoPrefix": false, "Advanced": false, "Exclusive": false, "Sensitive": true, "DefaultStr": "", "ValueStr": "", "Type": "string"},
        {"Name": "pass", "FieldName": "", "Help": "Password.", "Default": "", "Value": null, "Hide": 0, "Required": false, "IsPassword": true, "NoPrefix": false, "Advanced": false, "Exclusive": false, "Sensitive": false, "DefaultStr": "", "ValueStr": "", "Type": "string"},
        {"Name": "bearer_token_command", "FieldName": "", "Help": "Command to run to get a bearer token.", "Default": "", "Value": null, "Hide": 0, "Required": false, "IsPassword": false, "NoPrefix": false, "Advanced": true, "Exclusive": false, "Sensitive": false, "DefaultStr": "", "ValueStr": "", "Type": "string"}
      ]}]}"#;

    /// 测试替身:记录调用并按预设应答(mock 仅限测试且命名 Fake*)。
    struct FakeDispatch {
        reply: Result<Value, PartisyError>,
        calls: Mutex<Vec<(String, Value)>>,
    }

    impl FakeDispatch {
        fn ok(reply: Value) -> Self {
            FakeDispatch {
                reply: Ok(reply),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl RcDispatch for FakeDispatch {
        fn call(&self, method: &str, params: &Value) -> Result<Value, PartisyError> {
            self.calls
                .lock()
                .expect("fake call log")
                .push((method.to_owned(), params.clone()));
            // 预设应答不可克隆(PartisyError),按次数搬运包装。
            match &self.reply {
                Ok(value) => Ok(value.clone()),
                Err(err) => Err(PartisyError::with_source(
                    err.severity,
                    Box::new(ForwardedError(err.to_string())),
                )),
            }
        }
    }

    /// 错误源替身:模拟引擎错误文本(可携带被测敏感串)。
    #[derive(Debug)]
    struct ForwardedError(String);

    impl fmt::Display for ForwardedError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(&self.0)
        }
    }

    impl std::error::Error for ForwardedError {}

    /// 错误应答替身构造(固定 Fatal,severity 由用例断言)。
    fn fake_error(text: &str) -> FakeDispatch {
        FakeDispatch {
            reply: Err(PartisyError::with_source(
                Severity::Fatal,
                Box::new(ForwardedError(text.to_owned())),
            )),
            calls: Mutex::new(Vec::new()),
        }
    }

    #[test]
    fn webdav_fixture_parses_into_card_field_model() {
        // 卡内 ①:真机 fixture → 七属性模型;未知字段(FieldName/Sensitive/
        // Examples/Help 等)宽容忽略不破解析;Type 原样透传;默认值类型化。
        let forms = parse_provider_forms(serde_json::from_str(WEBDAV_FIXTURE).expect("fixture"))
            .expect("fixture parses");
        assert_eq!(forms.len(), 1);
        let form = &forms[0];
        assert_eq!(
            (form.name.as_str(), form.description.as_str()),
            ("webdav", "WebDAV")
        );
        assert_eq!(form.fields.len(), 5);
        let url = form.field("url").expect("url field");
        assert_eq!(url.field_type, "string");
        assert!(url.required && !url.is_password && !url.advanced && !url.exclusive);
        assert_eq!(url.default, json!(""));
        let vendor = form.field("vendor").expect("vendor field");
        assert!(!vendor.exclusive && !vendor.required && vendor.default == json!(""));
        let pass = form.field("pass").expect("pass field");
        assert!(pass.is_password && !pass.required);
        let command = form.field("bearer_token_command").expect("command field");
        assert!(command.advanced);
        // 按名查找 miss 面。
        assert!(form.field("nope").is_none());
        assert!(provider_form(&forms, "nope").is_none());
    }

    #[test]
    fn shape_drift_and_missing_provider_are_structured_fatal() {
        // 卡内 ①:协议漂移(缺 providers 键/类型不符)= 结构化 Fatal 零吞错;
        // 未命中 provider = ProviderNotFound(Fatal)。
        for broken in [json!({}), json!({"providers": {}}), json!({"providers": 3})] {
            let err = parse_provider_forms(broken).expect_err("shape drift rejected");
            assert_eq!(err.severity, Severity::Fatal);
            assert!(matches!(
                kind_of(&err),
                Some(SchemaErrorKind::ResponseInvalid(_))
            ));
        }
        let dispatch = FakeDispatch::ok(serde_json::from_str(WEBDAV_FIXTURE).expect("fixture"));
        let err = form_for_provider(&dispatch, "nope").expect_err("missing provider");
        assert_eq!(err.severity, Severity::Fatal);
        assert_eq!(
            kind_of(&err),
            Some(&SchemaErrorKind::ProviderNotFound("nope".to_owned()))
        );
        // 命中路径:同一替身记录了一次 config/providers 调用(白名单方法名)。
        assert_eq!(
            dispatch.calls.lock().expect("call log")[0].0.as_str(),
            "config/providers"
        );
    }

    #[test]
    fn create_channel_sends_card_shape_and_debug_is_value_free() {
        // 卡内 ②③:提交形状 = {name,type,parameters,obscure:true}(方法名
        // config/create 在白名单,由适配器二次收口);Debug 只出键名零值。
        let dispatch = FakeDispatch::ok(json!({}));
        let remote = RemoteCreate::new("pvtest", "webdav")
            .with_parameter("url", "https://webdav.example.invalid/webdav")
            .with_parameter("pass", "s3cr3t-pass-value");
        create_remote(&dispatch, &remote).expect("create ok");
        let calls = dispatch.calls.lock().expect("call log");
        let (method, params) = &calls[0];
        assert_eq!(method.as_str(), "config/create");
        assert_eq!(params["name"], "pvtest");
        assert_eq!(params["type"], "webdav");
        assert_eq!(params["obscure"], true);
        assert_eq!(
            params["parameters"]["url"],
            "https://webdav.example.invalid/webdav"
        );
        assert_eq!(params["parameters"]["pass"], "s3cr3t-pass-value");
        let debug = format!("{remote:?}");
        assert!(debug.contains("url") && debug.contains("pass"));
        assert!(!debug.contains("s3cr3t-pass-value"));
        assert!(!debug.contains("example.invalid"));
    }

    #[test]
    fn create_failure_carries_redacted_detail_with_engine_severity() {
        // 卡内 ③:错误载荷 = 引擎文本经全部参数值 redact 兜底(模拟 rc 错误
        // 措辞回显参数值的最坏情形);severity 保真;零吞错(非凭据根因保留)。
        let dispatch = fake_error(
            "couldn't find backend for type \"nosuchbackend\": url=leak pass=s3cr3t-pass-value",
        );
        let remote = RemoteCreate::new("pvtest", "nosuchbackend")
            .with_parameter("url", "https://webdav.example.invalid/webdav")
            .with_parameter("pass", "s3cr3t-pass-value");
        let err = create_remote(&dispatch, &remote).expect_err("create rejected");
        assert_eq!(err.severity, Severity::Fatal);
        let rejection = rejection_of(&err).expect("structured rejection");
        assert_eq!(rejection.remote, "pvtest");
        assert!(rejection.detail.contains("nosuchbackend"));
        assert!(!rejection.detail.contains("s3cr3t-pass-value"));
        assert!(!err.to_string().contains("s3cr3t-pass-value"));
        // 非 create 通道来源的错误 → None。
        assert!(rejection_of(&PartisyError::new(Severity::Fatal)).is_none());
        // 提交面零表单值泄漏(请求参数只经 rc 通道,不进错误以外的任何文本)。
        assert!(err.to_string().contains("remote \"pvtest\""));
    }
}
