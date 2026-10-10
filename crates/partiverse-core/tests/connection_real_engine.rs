//! M1-WP04-T03 卡内 DoD ⑤ 集成测试(真实 rclone 1.75.1,离线):webdav
//! schema 拉取 → 表单模型生成 → 123 形态 remote 创建(config/create)→
//! `config/get` 回读字段一致 → 密码不落 dump/错误(卡内 ②③密码脱敏断言)。
//! 端点 = 测试内伪 https 端点(.invalid 保留域,创建路径零触网)。
// unix socket 门:引擎协调器与 rc 通道仅 unix(架构 §3 禁 TCP,同 T01/T02 集成口径)。
#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use partiverse_core::error::PartisyError;
use partiverse_core::jobs::RcDispatch;
use partiverse_core::pan123::webdav_remote;
use partiverse_core::schema::{create_remote, form_for_provider, provider_forms, rejection_of};
use partiverse_engine::{
    DEFAULT_SLOT_ID, EngineCoordinator, EngineInstaller, EngineManifest, EngineSlotConfig,
    RcClient, RcMethod,
};
use serde_json::{Value, json};

/// 集成测试进程内串行(引擎安装根共享路径,同 WP03 集成纪律)。
fn test_serial_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// T01 RcClient → core RcDispatch 适配器(生产消费面形态证明,WP03 同款)。
struct RcAdapter {
    client: RcClient,
}

impl RcDispatch for RcAdapter {
    fn call(&self, method: &str, params: &Value) -> Result<Value, PartisyError> {
        let method = RcMethod::try_from_name(method)
            .map_err(|err| PartisyError::with_source(err.severity(), Box::new(err)))?;
        self.client
            .call(method, params)
            .map_err(|err| PartisyError::with_source(err.severity(), Box::new(err)))
    }
}

/// 预装引擎预检(防误触网络下载;缺失 = 显式失败+安装指引,WP03 同款)。
fn require_preinstalled_engine() -> PathBuf {
    let root = EngineInstaller::resolve_install_root().expect("engine install root resolvable");
    let manifest = EngineManifest::embedded().expect("embedded manifest parses");
    let asset = manifest.current_platform().expect("host platform asset");
    let binary = root.join(&manifest.version).join(&asset.binary_name);
    assert!(
        binary.is_file(),
        "rclone missing at {}; run engine-install",
        root.display()
    );
    binary
}

/// 本卡消费的方法全集均在 T01 白名单(卡内 ②「白名单内」真机锚定)。
fn assert_methods_whitelisted() {
    for method in [
        "config/providers",
        "config/create",
        "config/get",
        "config/dump",
        "config/delete",
    ] {
        assert!(
            RcMethod::try_from_name(method).is_ok(),
            "{method} not whitelisted"
        );
    }
}

#[test]
fn schema_model_create_readback_and_password_never_leaks_on_real_engine() {
    let _serial = test_serial_lock().lock().expect("serial lock");
    // 预装预检(防误触网络下载):缺失即显式失败+安装指引。
    require_preinstalled_engine();
    assert_methods_whitelisted();

    let work = std::env::temp_dir().join(format!("partiverse-wp04-t03-{}", std::process::id()));
    let mut slot = EngineSlotConfig::load_default(DEFAULT_SLOT_ID).expect("default slot loads");
    slot.cache_dir = work.join("cache");
    fs::create_dir_all(&slot.cache_dir).expect("create test cache dir");

    let mut coordinator = EngineCoordinator::new().expect("coordinator constructs");
    let handle = coordinator.ensure(&slot).expect("ensure engine");
    let adapter = RcAdapter {
        client: handle.rc_client.clone(),
    };

    // 卡内 ①:真机 schema 拉取 → 表单模型生成(形状与单测 fixture 同锚定断言)。
    let forms = provider_forms(&adapter).expect("config/providers ok");
    assert!(forms.len() >= 60, "provider count sanity: {}", forms.len());
    let form = form_for_provider(&adapter, "webdav").expect("webdav form present");
    assert_eq!(
        (form.name.as_str(), form.description.as_str()),
        ("webdav", "WebDAV")
    );
    let url = form.field("url").expect("url field");
    assert!(url.required && url.field_type == "string" && !url.is_password && !url.advanced);
    let vendor = form.field("vendor").expect("vendor field");
    assert!(!vendor.exclusive && !vendor.required);
    let pass = form.field("pass").expect("pass field");
    assert!(pass.is_password && !pass.required);

    // 卡内 ③:123 形态 remote(端点用户粘贴语义——测试用伪端点,零硬编码)。
    const ENDPOINT: &str = "https://webdav.example.invalid/webdav";
    const ACCOUNT: &str = "13800000000";
    const APP_PASS: &str = "wp04t03-app-pass-9f3a";
    let remote = webdav_remote("pvwp04t03", ENDPOINT, ACCOUNT, APP_PASS).expect("123 form builds");
    create_remote(&adapter, &remote).expect("config/create ok");

    // 卡内 ⑤:config/get 回读字段一致;密码字段 = obscured 形态(非明文)。
    let got = adapter
        .call("config/get", &json!({ "name": "pvwp04t03" }))
        .expect("config/get ok");
    assert_eq!(got["type"], "webdav");
    assert_eq!(got["url"], ENDPOINT);
    assert_eq!(got["user"], ACCOUNT);
    assert_eq!(got["vendor"], "other");
    let stored_pass = got["pass"].as_str().expect("pass field present");
    assert!(!stored_pass.is_empty(), "pass stored");
    assert_ne!(stored_pass, APP_PASS, "pass stored obscured, not plaintext");

    // 卡内 ②⑤:密码不落 dump(rc 全量配置面;obsurred ≠ 明文)。
    let dump = adapter
        .call("config/dump", &json!({}))
        .expect("config/dump ok");
    assert!(
        !dump.to_string().contains(APP_PASS),
        "plaintext app password must never appear in config dump"
    );

    // 卡内 ③:失败路径错误载荷零明文(未知 backend 触发 5xx;RcClient 只取
    // error 字段,通道再经参数值 redact 兜底——实测 rc 错误回包 input 回显
    // 全部入参,此处端到端证明明细不破零明文红线)。severity 沿引擎分级。
    let mut bad = remote.clone();
    bad.remote_type = "nosuchbackend".to_owned();
    let err = create_remote(&adapter, &bad).expect_err("unknown backend rejected");
    let text = err.to_string();
    assert!(
        text.contains("nosuchbackend"),
        "non-secret root cause kept: {text}"
    );
    assert!(
        !text.contains(APP_PASS) && !text.contains(ENDPOINT),
        "error text must not carry parameter values: {text}"
    );
    assert!(rejection_of(&err).is_some(), "structured rejection payload");

    // 收尾:config/delete 清场并确认(白名单内;零残留)。get 对已删 remote
    // 返回 200+空对象(1.75.1 实测语义:空=不存在),据此断言零残留。
    adapter
        .call("config/delete", &json!({ "name": "pvwp04t03" }))
        .expect("cleanup config/delete");
    let gone = adapter
        .call("config/get", &json!({ "name": "pvwp04t03" }))
        .expect("config/get ok");
    assert_eq!(gone, json!({}), "remote deleted (empty readback)");

    coordinator
        .shutdown(DEFAULT_SLOT_ID)
        .expect("shutdown engine");
    let _ = fs::remove_dir_all(&work);
}
