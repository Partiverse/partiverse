//! connection 子命令(M1-WP07-T01 卡内 ②):create-local / create-webdav /
//! list / delete,全部透传 crates(core `schema::create_remote` 注入通道 +
//! `pan123::webdav_remote` 形态构造 + rc 白名单 config/*),CLI 零业务逻辑。
//! 凭据红线:`--pass` 参数全程零日志零错误消息(core 注入通道错误路径已对全部
//! 参数值 redact 兜底);connection list 只出 name+type,零参数值。

use partiverse_core::error::{PartisyError, Severity};
use partiverse_core::jobs::RcDispatch;
use partiverse_core::pan123::webdav_remote;
use partiverse_core::schema::{RemoteCreate, create_remote};
use serde_json::{Value, json};

/// 结构化 Fatal 构造(InMemoryTokenSink 壳面先例:std io 错误为 source 根因)。
fn fatal(message: impl Into<String>) -> PartisyError {
    PartisyError::with_source(
        Severity::Fatal,
        Box::new(std::io::Error::other(message.into())),
    )
}

/// `connection create-local <name> --path <path>`:本地目录接入,经 alias 后端
/// 落 config/core `create_remote` 注入通道(透传,零 CLI 侧逻辑)。
///
/// # 后端选型实锚(2026-10-11,宿主件 = 引擎钉定 rclone v1.75.1,禁凭记忆)
/// `rclone help backend local` 实测:local 后端无 root 配置项(路径不能落在
/// config);`help backend alias` 实测:`remote` 配置项 Required:true,取值
/// 可为 `/local/path`;M1-WP05-T09 实测 alias 后端 `parameters:{"remote":…}`
/// 经 `config/create` → 200。故 create-local = alias 后端包本地路径,`--path`
/// 用户粘贴零硬编码(红线)。
///
/// # Errors
/// 路径/名称非法或 `config/create` 被拒 → core 错误原样上浮(参数值已 redact)。
pub fn create_local(
    dispatch: &impl RcDispatch,
    name: &str,
    path: &str,
) -> Result<(), PartisyError> {
    let remote = RemoteCreate::new(name, "alias").with_parameter("remote", path.to_owned());
    create_remote(dispatch, &remote)?;
    assert_type_readback(dispatch, name, "alias")
}

/// `connection create-webdav <name> --url --user --pass`:123 官方 WebDAV 形态
/// (core `pan123::webdav_remote` 构造:url/vendor/user/pass 四参数;端点用户
/// 粘贴零硬编码,密码逐字符保真)→ core `create_remote` 注入(obscure 落加密
/// config;config 加密接线不在本卡,D27 过渡件债务与壳层同口径)。
///
/// # Errors
/// 表单校验失败(端点非 https 等)或 `config/create` 被拒 → core 错误原样上浮
/// (错误载荷零用户粘贴值,core 已脱敏)。
pub fn create_webdav(
    dispatch: &impl RcDispatch,
    name: &str,
    url: &str,
    user: &str,
    pass: &str,
) -> Result<(), PartisyError> {
    let remote = webdav_remote(name, url, user, pass)?;
    create_remote(dispatch, &remote)?;
    assert_type_readback(dispatch, name, "webdav")
}

/// `connection list`:rc `config/dump`(白名单内)回读,只出 `name<TAB>type`
/// 排序清单——dump 载荷含 obscured 密码,只取两键零参数值输出(凭据红线)。
///
/// # Errors
/// rc 失败或 dump 形状异常(非对象)→ 原样/结构化上浮(零吞错)。
pub fn list(dispatch: &impl RcDispatch) -> Result<String, PartisyError> {
    let dump = dispatch.call("config/dump", &json!({}))?;
    let object = dump
        .as_object()
        .ok_or_else(|| fatal("config/dump reply is not an object"))?;
    let mut lines = Vec::new();
    for (name, entry) in object {
        let remote_type = entry
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("<no type>");
        lines.push(format!("{name}\t{remote_type}"));
    }
    lines.sort();
    Ok(lines.join("\n"))
}

/// `connection delete <name>`:rc `config/delete`(白名单内)→ `config/get`
/// 回读断言已删(1.75.1 实测语义:已删 remote 回 200+空对象,M1-WP05-T09 锚)。
///
/// # Errors
/// rc 失败原样上浮;回读非空 = 删除未生效,结构化 Fatal 上浮(零静默)。
pub fn delete(dispatch: &impl RcDispatch, name: &str) -> Result<(), PartisyError> {
    dispatch.call("config/delete", &json!({ "name": name }))?;
    let readback = dispatch.call("config/get", &json!({ "name": name }))?;
    if readback != Value::Object(serde_json::Map::new()) {
        // 回读非空 = 删除未生效:只报事实(名非凭据),零参数值入载荷。
        return Err(fatal(format!(
            "config/delete accepted but readback for `{name}` still non-empty"
        )));
    }
    Ok(())
}

/// `config/get` 回读断言 `type` 一致(M1-WP05-T09 壳单点收口同款纪律:真建
/// remote → 回读;回包扁平配置恒含 `type`,实测锚定)。
///
/// # Errors
/// rc 失败原样上浮;`type` 不符 → 结构化 Fatal(只报事实键值,零参数值)。
fn assert_type_readback(
    dispatch: &impl RcDispatch,
    name: &str,
    expected_type: &str,
) -> Result<(), PartisyError> {
    let reply = dispatch.call("config/get", &json!({ "name": name }))?;
    let readback = reply.get("type").and_then(Value::as_str);
    if readback != Some(expected_type) {
        // 回读断言失败:只报事实键值(type 非凭据),零参数值入载荷。
        return Err(fatal(format!(
            "config/get readback type mismatch for remote `{name}`: expected \
             `{expected_type}`, got `{}`",
            readback.unwrap_or("<missing>")
        )));
    }
    Ok(())
}
