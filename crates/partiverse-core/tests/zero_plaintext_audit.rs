//! M1-WP04-T04 凭据零明文审计套件(随仓断言测试)。
//!
//! 形态裁定(卡内 ①「测试化或 xtask 子命令二选一并说明」):**测试化**。
//! 理由:①门禁前置(铁律 5)——`cargo test` 已在五门禁,审计随每次提交自动
//! 执行,xtask 子命令需人工触发、存在漏跑面;②「命中即失败并指向行」与测试
//! 失败信息(file:line + 规则名)直接同构;③零新依赖、零子命令维护面。
//!
//! 三段(卡内 ①②③):
//! ① 静态扫描:crates/*/src 生产代码(.rs;剥整行/行尾注释,`#[cfg(test)]`
//!    模块块跳过)按规则表匹配禁止模式(密钥字面量/硬编码端点+凭据组合/
//!    rclone obscure 调用/凭据进日志宏),命中即失败并指向 file:line;误报
//!    白名单逐条显式注释。规则为模式级而非语义级——语义审计归双通道评审
//!    (铁律 6/7),此为卡内边界声明。xtask 不在扫描面(卡内声明扫 crates/)。
//! ② 日志脱敏端到端:OAuth 换码全链(127.0.0.1 回环 fixture,真实生产函数,
//!    零真实网络)→ 123 WebDAV 载体 → config/create 提交 → 全链路诊断流经
//!    [`partiverse_core::credential_store::redact`] 过滤器,secret/password/
//!    token/code 四类字段值字节级零明文 + 阳性对照防空洞。加密 config 三断言
//!    复用 T01 `tests/config_encryption_real_engine.rs`(勿重写,本卡运行取证)。
//! ③ 遥测面:`cargo tree --locked` 全树断言零 analytics/telemetry/sentry 类
//!    依赖(合规红线 4:凭据禁入遥测崩溃报告;架构 §5)。
//!
//! 全程离线:HTTP 仅 127.0.0.1 回环 fixture;keychain 用内存 Fake(真实
//! keychain 行为面归 T01 集成测试与 Owner 实测,本卡不重复)。

use std::fmt;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use partiverse_core::credential_store::{self, CredentialErrorKind, KeyringStore};
use partiverse_core::error::{PartisyError, Severity};
use partiverse_core::jobs::RcDispatch;
use partiverse_core::oauth::{
    BaiduOAuthFlow, ClientCredentials, OAuthErrorKind, OAuthTokenSink, TokenSet, kind_of,
};
use partiverse_core::schema::{self, rejection_of};
use serde_json::Value;

// ──────────────────────────── ① 静态扫描 ────────────────────────────

/// 凭据语义关键字,空格分隔(词左边界匹配;`client_secret`/`access_token` 等
/// 经 `secret`/`token` 子词覆盖,`BAIDU_TOKEN_ENDPOINT` 类下划线复合名不误伤)。
const SECRET_KEYWORDS: &str = "password passwd passphrase secret token api_key apikey app_key";

/// URL 内嵌凭据赋值形态(空格分隔)。
const URL_CREDENTIAL_ASSIGN: &str =
    "password= passwd= passphrase= secret= token= api_key= apikey= app_key= client_secret=";

/// 日志宏标记(`print!` 与 `println!` 互不为子串,须逐个列举;空格分隔)。
const LOG_MACRO_MARKS: &str =
    "println! print! eprintln! eprint! log:: tracing:: trace! debug! info! warn! error!";

/// 审计规则表(卡内「模式可列举」;每条 = 名称 + 依据 + 匹配器)。
struct Rule {
    name: &'static str,
    rationale: &'static str,
    hits: fn(&str) -> bool,
}

const RULES: &[Rule] = &[
    Rule {
        name: "credential_literal_binding",
        rationale: "密钥字面量:凭据语义标识符绑定字符串字面量(卡内 ①)",
        hits: keyword_binding_hit,
    },
    Rule {
        name: "endpoint_plus_credential_binding",
        rationale: "硬编码端点+凭据组合:同行出现 http(s) URL 与凭据字面量绑定(卡内 ①)",
        hits: |line| {
            (line.contains("http://") || line.contains("https://")) && keyword_binding_hit(line)
        },
    },
    Rule {
        name: "credential_embedded_in_url",
        rationale: "凭据内嵌 URL 字面量(password=/token=/secret= 形态;卡内 ①)",
        hits: |line| {
            (line.contains("http://") || line.contains("https://"))
                && URL_CREDENTIAL_ASSIGN
                    .split(' ')
                    .any(|mark| line.contains(mark))
        },
    },
    Rule {
        name: "obscure_as_encryption",
        rationale: "rclone obscure 调用面(架构 §5:obscure 是混淆非加密,禁作凭据保护手段)",
        hits: |line| line.contains("obscure"),
    },
    Rule {
        name: "credential_into_log_macro",
        rationale: "凭据序列化进日志宏(卡内 ①;token/secret/password 类关键字 + 日志宏)",
        hits: |line| {
            LOG_MACRO_MARKS.split(' ').any(|mark| line.contains(mark))
                && SECRET_KEYWORDS
                    .split(' ')
                    .any(|keyword| keyword_word_present(line, keyword))
        },
    },
    Rule {
        name: "hex_secret_literal",
        rationale: "≥32 位 hex 串字面量(密钥形态;发行物 sha256 完整性锚若命中须白名单显式豁免)",
        hits: hex_literal_hit,
    },
];

/// 误报白名单(卡内「误报白名单显式注释」):(文件名, 精确行(trimmed),
/// 规则名, 豁免理由)。当前两条;新增须先人工核实确属误报。
const WHITELIST: &[(&str, &str, &str, &str)] = &[
    (
        "schema.rs",
        "\"obscure\": true,",
        "obscure_as_encryption",
        "rc `config/create` 的 obscure 参数显式化(M1-WP04-T03 卡内 ②③ 已批,防上游默认\
         变化):密码由 rclone 在写入加密 config 前自行 obscure,系 rclone 内部写入步骤;\
         架构 §5 禁止的是代码直接调用 `rclone obscure` 并把其输出当加密持久化,非本参数",
    ),
    (
        "rcd.rs",
        "const PASSWORD_COMMAND_ENV: &str = \"RCLONE_PASSWORD_COMMAND\";",
        "credential_literal_binding",
        "rclone 官方文档记载的环境变量名常量(RCLONE_PASSWORD_COMMAND 的名字本身),\
         值为变量名非凭据;密钥本体仅经取密脚本/stdin 传递,T01 已断言进程 argv/env \
         零密钥(credential_store 单测 rclone_commands_carry_zero_secret_in_args_or_env)",
    ),
];

/// 扫描命中(file:line + 规则 + 原行)。
#[derive(Debug)]
struct ScanHit {
    location: String,
    rule: &'static str,
    line: String,
}

#[test]
fn static_scan_production_sources_zero_hits() {
    let root = workspace_root();
    let crates_dir = root.join("crates");
    assert!(
        crates_dir.is_dir(),
        "scan root missing: {}",
        crates_dir.display()
    );
    let mut files = Vec::new();
    // 扫描面 = crates/*/src 生产树(卡内「扫 crates/ 生产代码」);tests/
    // 与 benches/ 不属生产代码,不入扫描面。
    for entry in std::fs::read_dir(&crates_dir)
        .unwrap_or_else(|err| panic!("audit cannot list {}: {err}", crates_dir.display()))
        .flatten()
    {
        let src_dir = entry.path().join("src");
        if src_dir.is_dir() {
            collect_rs_files(&src_dir, &mut files);
        }
    }
    files.sort();
    // 非空洞护栏:扫描面须覆盖三个 crate 的生产树,文件数异常即红。
    assert!(
        files.len() >= 15,
        "scan surface suspiciously small ({} files under crates/*/src)",
        files.len()
    );

    let mut hits = Vec::new();
    for path in &files {
        let content = std::fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("audit cannot read {}: {err}", path.display()));
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string();
        scan_file(&rel, &content, &mut hits);
    }
    let report = hits
        .iter()
        .map(|hit| {
            format!(
                "\n  [{}] {} -> {}\n    line: {}",
                hit.rule,
                hit.location,
                rule_rationale(hit.rule),
                hit.line
            )
        })
        .collect::<String>();
    assert!(
        hits.is_empty(),
        "零明文静态审计命中 {} 处(每处须修复或经显式白名单):{report}",
        hits.len()
    );
}

/// 规则匹配器自锚定(审计器自身非空洞):每条规则一对必中/必不中样本。
#[test]
fn audit_rules_fire_on_violation_samples_and_stay_quiet_on_benign_lines() {
    // credential_literal_binding
    assert!(keyword_binding_hit(&lower(
        "let password = \"hunter2hunter2\";"
    )));
    assert!(keyword_binding_hit(&lower(
        "const API_KEY: &str = \"sk-live-abcdef123456\";"
    )));
    assert!(!keyword_binding_hit(&lower(
        "let db_password = read_from_keychain()?;"
    )));
    // 端点常量命名含 token 子词但零凭据绑定 → 不误伤。
    assert!(!keyword_binding_hit(&lower(
        "const BAIDU_TOKEN_ENDPOINT: &str = \"https://openapi.baidu.com/oauth/2.0/token\";"
    )));
    // endpoint_plus_credential_binding(同行 URL + 凭据绑定)
    let combo = lower("let url = \"https://api.example.test\"; let password = \"hunter2hunter2\";");
    assert!((RULES[1].hits)(&combo));
    assert!(!(RULES[1].hits)(&lower(
        "let url = \"https://api.example.test/v1\";"
    )));
    // credential_embedded_in_url
    assert!((RULES[2].hits)(&lower(
        "build(\"https://host.example/p?token=LIVEVALUE12345\")"
    )));
    assert!(!(RULES[2].hits)(&lower(
        "let base = \"https://openapi.baidu.com/oauth/2.0/token\";"
    )));
    // obscure_as_encryption
    assert!((RULES[3].hits)(&lower("run(\"rclone\", \"obscure\")")));
    assert!(!(RULES[3].hits)(&lower("let unrelated = 1;")));
    // credential_into_log_macro
    assert!((RULES[4].hits)(&lower(
        "println!(\"token {}\", raw_value);"
    )));
    assert!(!(RULES[4].hits)(&lower(
        "println!(\"engine ready: {}\", path.display());"
    )));
    // hex_secret_literal
    assert!((RULES[5].hits)(
        "const K: &str = \"0123456789abcdef0123456789abcdef\";"
    ));
    assert!(!(RULES[5].hits)("let short = \"deadbeef\";"));
}

/// 扫描器解析假设自锚定:注释剥离与 cfg(test) 模块跳过行为。
#[test]
fn scanner_strips_comments_and_skips_cfg_test_modules() {
    let file = "\
// 注释里的 password = \"leakleakleak\" 不扫描(整行注释)
let ok_line = read_password_from_keychain();
let bad_line = token_value = \"abcdefabcdef\"; // 注释剥离后仍应命中
#[cfg(test)]
mod tests {
    let tests_fixture_password = \"deadbeefdeadbeef\";
}
let after_tests_bad = api_key = \"123456123456\";
";
    let mut hits = Vec::new();
    scan_file("synthetic.rs", file, &mut hits);
    assert_eq!(hits.len(), 2, "须恰好命中 mod 外两处: {hits:?}");
    assert_eq!(hits[0].rule, "credential_literal_binding");
    assert_eq!(
        hits[1].line,
        "let after_tests_bad = api_key = \"123456123456\";"
    );
}

fn scan_file(rel: &str, content: &str, hits: &mut Vec<ScanHit>) {
    let mut in_test_mod = false;
    let mut brace_depth: i64 = 0;
    let mut attr_pending = false;
    for (idx, raw) in content.lines().enumerate() {
        let no = idx + 1;
        if in_test_mod {
            // 花括号深度计数(含字符串内花括号——测试 fixture JSON 恒平衡;
            // 若提前失衡只会把测试模块当生产行扫出多余命中,偏差方向安全)。
            brace_depth += count_byte(raw, b'{') as i64 - count_byte(raw, b'}') as i64;
            if brace_depth <= 0 {
                in_test_mod = false;
            }
            continue;
        }
        let trimmed = raw.trim();
        if trimmed.starts_with("//") {
            continue; // 整行注释不扫描
        }
        let code = strip_line_comment(raw);
        let line = code.trim();
        if line.is_empty() {
            continue;
        }
        if attr_pending {
            if line.starts_with("mod ") {
                attr_pending = false;
                brace_depth = count_byte(line, b'{') as i64 - count_byte(line, b'}') as i64;
                if brace_depth > 0 {
                    in_test_mod = true; // `mod tests {`(单行空块深度 0,直接跳过)
                }
                continue;
            }
            attr_pending = false; // #[cfg(test)] 未跟 mod(如单行 use)→ 落回常规扫描
        }
        if line.starts_with("#[cfg(test)]") {
            if line.contains("mod ") {
                // 同行形态 `#[cfg(test)] mod tests {`。
                brace_depth = count_byte(line, b'{') as i64 - count_byte(line, b'}') as i64;
                if brace_depth > 0 {
                    in_test_mod = true;
                }
            } else {
                attr_pending = true;
            }
            continue;
        }
        // 解析假设:rustfmt 规约代码(属性独行、mod 花括号显式);非常规排版
        // 的偏差只会把测试行当生产行扫出多余命中(误报可见),不会漏扫生产行。
        let lowered = lower(line);
        for rule in RULES {
            if (rule.hits)(&lowered) && !whitelisted(rel, rule.name, line) {
                hits.push(ScanHit {
                    location: format!("{rel}:{no}"),
                    rule: rule.name,
                    line: line.to_owned(),
                });
            }
        }
    }
}

fn whitelisted(rel: &str, rule: &str, line: &str) -> bool {
    WHITELIST.iter().any(|(file, expected, rule_name, _)| {
        rel.ends_with(file) && rule == *rule_name && line == *expected
    })
}

fn rule_rationale(name: &str) -> &'static str {
    RULES
        .iter()
        .find(|rule| rule.name == name)
        .map_or("unknown rule", |rule| rule.rationale)
}

/// 凭据关键字出现点是否构成「标识符 → 字符串字面量」绑定(容忍
/// `: &str = "..."` 类型标注;`==` 比较与无引号赋值不判,漏报面归评审通道)。
fn keyword_binding_hit(line_lower: &str) -> bool {
    for keyword in SECRET_KEYWORDS.split(' ') {
        let mut from = 0;
        while let Some(rel) = line_lower[from..].find(keyword) {
            let start = from + rel;
            let end = start + keyword.len();
            if left_boundary_ok(&line_lower[..start]) && binds_string_literal(line_lower, end) {
                return true;
            }
            from = end;
        }
    }
    false
}

fn keyword_word_present(line_lower: &str, keyword: &str) -> bool {
    let mut from = 0;
    while let Some(rel) = line_lower[from..].find(keyword) {
        let start = from + rel;
        if left_boundary_ok(&line_lower[..start]) {
            return true;
        }
        from = start + keyword.len();
    }
    false
}

fn left_boundary_ok(prefix: &str) -> bool {
    match prefix.chars().next_back() {
        Some(c) => !(c.is_ascii_alphanumeric() || c == '_'),
        None => true,
    }
}

fn binds_string_literal(line_lower: &str, keyword_end: usize) -> bool {
    let rest = &line_lower[keyword_end..];
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() {
        return false;
    }
    match bytes[i] {
        b'=' if bytes.get(i + 1) != Some(&b'=') => {}
        b'=' => return false, // `==` 比较,非绑定
        b':' => {
            let Some(eq_rel) = rest[i + 1..].find('=') else {
                return false;
            };
            let between = &rest[i + 1..i + 1 + eq_rel];
            // 类型标注段(`&str`/`String` 形态)不允许出现引号或语句边界
            // (防 `secret: X, other = "..."` 跨段误判)。
            if between.contains('"') || between.contains(',') || between.contains(';') {
                return false;
            }
            i += 1 + eq_rel;
        }
        _ => return false,
    }
    rest[i + 1..].trim_start().starts_with('"')
}

fn hex_literal_hit(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
                j += 1;
            }
            if j - (i + 1) >= 32 && j < bytes.len() && bytes[j] == b'"' {
                return true;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    false
}

/// 引号感知的行尾 `//` 注释剥离(字符串字面量内的 `//` 不切)。
fn strip_line_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => in_string = !in_string,
            b'\\' if in_string => i += 1,
            b'/' if !in_string && bytes.get(i + 1) == Some(&b'/') => return &line[..i],
            _ => {}
        }
        i += 1;
    }
    line
}

fn count_byte(text: &str, byte: u8) -> usize {
    text.bytes().filter(|b| *b == byte).count()
}

fn lower(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).collect()
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

// ──────────────────────────── ③ 遥测面断言 ────────────────────────────

/// 遥测/崩溃上报类依赖模式表(合规红线 4:凭据禁入遥测崩溃报告;架构 §5)。
/// opentelemetry 含 telemetry 子串,由 telemetry 覆盖。
const TELEMETRY_PATTERNS: &str = "analytics telemetry sentry bugsnag datadog newrelic new-relic amplitude mixpanel posthog crashlytics appcenter app-center instabug crash-report";

#[test]
fn telemetry_dependency_surface_is_zero() {
    let root = workspace_root();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    // --locked:审计锚定已入库锁文件,只读(测试路径零锁文件改动);
    // 独立 CARGO_TARGET_DIR:与外层 cargo test 零构建锁交互(cargo tree 只解析)。
    let target_dir =
        std::env::temp_dir().join(format!("partiverse-wp04-t04-tree-{}", std::process::id()));
    let output = Command::new(cargo)
        .args(["tree", "--workspace", "--all-targets", "--locked"])
        .env("CARGO_TARGET_DIR", &target_dir)
        .current_dir(&root)
        .output()
        .expect("cargo tree must spawn");
    assert!(
        output.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8_lossy(&output.stdout).to_lowercase();
    // 非空洞护栏:树必须真解析出本仓 crate 与已知核心依赖。
    assert!(
        tree.contains("partiverse-core") && tree.contains("rusqlite"),
        "cargo tree output lost sanity (vacuous scan)"
    );
    let hits: Vec<&str> = TELEMETRY_PATTERNS
        .split(' ')
        .filter(|pattern| tree.contains(*pattern))
        .collect();
    assert!(hits.is_empty(), "遥测类依赖进树(零遥测断言失败): {hits:?}");
    let _ = std::fs::remove_dir_all(&target_dir);
}

// ──────────────────────── ② 日志脱敏端到端 ────────────────────────────

/// 密值常量(仅测试进程内存;零明文断言的「不出现」基准)。
const FAKE_CLIENT_ID: &str = "wp04t04-client-id";
const FAKE_CLIENT_SECRET: &str = "wp04t04-fake-client-secret";
const FAKE_CODE: &str = "wp04t04-fake-oauth-code";
const FAKE_ACCESS: &str = "wp04t04-fake-access-token";
const FAKE_REFRESH: &str = "wp04t04-fake-refresh-token";
const FAKE_ENDPOINT: &str = "https://webdav.123pan.invalid/pasted-by-user";
const FAKE_ACCOUNT: &str = "wp04t04-account@example.invalid";
const FAKE_APP_PASS: &str = "wp04t04-app-password-9f3a";

/// Fake 面结构化错误(测试专用;PartisyError 字段公开,直构零吞错)。
fn fake_fatal(kind: CredentialErrorKind) -> PartisyError {
    PartisyError {
        severity: Severity::Fatal,
        source: Some(Box::new(kind)),
    }
}

/// 测试替身 keychain(命名 Fake*,卡内边界):内存态,零真实用户数据。
#[derive(Debug, Default)]
struct FakeKeyring(Mutex<Option<String>>);

impl FakeKeyring {
    /// 锁统一映射(中毒 → 结构化 PlatformFailure,零吞错)。
    fn lock_slot(&self) -> Result<std::sync::MutexGuard<'_, Option<String>>, PartisyError> {
        self.0.lock().map_err(|poisoned| {
            fake_fatal(CredentialErrorKind::PlatformFailure(poisoned.to_string()))
        })
    }
}

impl KeyringStore for FakeKeyring {
    fn set(&self, secret: &str) -> Result<(), PartisyError> {
        *self.lock_slot()? = Some(secret.to_owned());
        Ok(())
    }

    fn get(&self) -> Result<String, PartisyError> {
        self.lock_slot()?
            .clone()
            .ok_or_else(|| fake_fatal(CredentialErrorKind::KeyMissing))
    }

    fn delete(&self) -> Result<(), PartisyError> {
        *self.lock_slot()? = None;
        Ok(())
    }
}

/// rc 错误回显体(模拟实测行为:rc 错误回包把 input 全参数回显,
/// schema.rs:263 记录的在先事实;命名 Fake*)。
#[derive(Debug)]
struct FakeEchoDetail(String);

impl fmt::Display for FakeEchoDetail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FakeEchoDetail {}

/// 测试替身 rc 通道(命名 Fake*):恒以回显全部入参的错误拒绝。
struct FakeEchoDispatch;

impl RcDispatch for FakeEchoDispatch {
    fn call(&self, method: &str, params: &Value) -> Result<Value, PartisyError> {
        Err(PartisyError::with_source(
            Severity::Fatal,
            Box::new(FakeEchoDetail(format!(
                "{method} rejected; input echoed: {params}"
            ))),
        ))
    }
}

/// 测试替身 token sink(命名 Fake*):内存保存,供断言。
#[derive(Default)]
struct FakeTokenSink(Mutex<Option<TokenSet>>);

impl OAuthTokenSink for FakeTokenSink {
    fn store_token(&self, token: &TokenSet) -> Result<(), PartisyError> {
        *self.0.lock().expect("fake sink mutex") = Some(token.clone());
        Ok(())
    }
}

/// 本地 fixture token 端点(std TcpListener,127.0.0.1 回环随机端口;零真实
/// 网络)。与 T02 fixture 同构但独立最小化(T02 版在其测试二进制内,跨测试
/// 二进制不可复用;此处仅固定应答+请求行记录,无延迟维度)。
struct FakeTokenEndpoint {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl FakeTokenEndpoint {
    fn spawn(status_line: &'static str, body: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake endpoint");
        let addr = listener.local_addr().expect("fake endpoint addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (log, shutdown) = (requests.clone(), Arc::new(AtomicBool::new(false)));
        let worker_shutdown = shutdown.clone();
        let worker = std::thread::spawn(move || {
            let _ = listener.set_nonblocking(true);
            // nonblocking 轮询 accept:shutdown 置位即退出,Drop 必达零悬挂。
            while !worker_shutdown.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                        let mut buffer = [0u8; 8192];
                        let n = stream.read(&mut buffer).unwrap_or(0);
                        if let Some(line) = String::from_utf8_lossy(&buffer[..n]).lines().next() {
                            log.lock().expect("request log").push(line.to_owned());
                        }
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
            url: format!("http://{addr}/oauth/2.0/token"),
            requests,
            shutdown,
            worker: Some(worker),
        }
    }

    /// fixture 端点 URL(回环 http,走 `with_token_endpoint` 测试注入缝)。
    fn url(&self) -> &str {
        &self.url
    }

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
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[test]
fn credential_chain_diagnostics_pass_redaction_filter_end_to_end() {
    // ── 阶段 1:keychain → config 主密钥(真实 CSPRNG 生成路径)──
    let keyring = FakeKeyring::default();
    let master = credential_store::ensure_master_key(&keyring).expect("master key ensured");

    // ── 阶段 2:OAuth 换码全链(真实生产函数;成功腿取回真 token)──
    let credentials =
        ClientCredentials::new(FAKE_CLIENT_ID, FAKE_CLIENT_SECRET).expect("credentials valid");
    let credentials_debug = format!("{credentials:?}");
    let success_body = format!(
        r#"{{"access_token":"{FAKE_ACCESS}","refresh_token":"{FAKE_REFRESH}","expires_in":2592000}}"#
    );
    let success_endpoint = FakeTokenEndpoint::spawn("200 OK", success_body);
    let flow = BaiduOAuthFlow::new(credentials)
        .with_token_endpoint(success_endpoint.url())
        .expect("loopback fixture endpoint accepted");
    let state = partiverse_core::oauth::generate_state().expect("state generated");
    let _authorize_url = flow.authorize_url(&state).expect("authorize url built");
    let token = flow
        .exchange_code(FAKE_CODE, &state)
        .expect("exchange succeeds against fixture");
    // 阳性对照:请求链真实携带 code+secret(query),脱敏面因此非空洞。
    let request_line = success_endpoint.first_request_line();
    assert!(
        request_line.contains(FAKE_CODE) && request_line.contains(FAKE_CLIENT_SECRET),
        "fixture 请求须携带 code/secret(阳性对照): {request_line}"
    );
    // 下沉契约:token 真值达 sink(生产 sink 接缝);取值面与 Debug 面断言。
    let sink = FakeTokenSink::default();
    sink.store_token(&token).expect("sink store");
    let stored = sink
        .0
        .lock()
        .expect("fake sink mutex")
        .clone()
        .expect("token stored");
    assert_eq!(stored.access_token, FAKE_ACCESS);
    assert_eq!(stored.refresh_token, FAKE_REFRESH);
    drop(success_endpoint);

    let token_debug = format!("{token:?}");
    assert!(
        !token_debug.contains(FAKE_ACCESS) && !token_debug.contains(FAKE_REFRESH),
        "TokenSet Debug 零 token 明文: {token_debug}"
    );
    assert!(
        !credentials_debug.contains(FAKE_CLIENT_SECRET),
        "ClientCredentials Debug 零 secret 明文: {credentials_debug}"
    );

    // ── 阶段 2b:provider 2xx 错误体 error 字段回显 code/secret →
    //    生产 redact 缝(oauth.rs `parse_token_response`)──
    let echo_body = format!(
        r#"{{"error":"invalid_grant: code {FAKE_CODE} secret {FAKE_CLIENT_SECRET} echoed"}}"#
    );
    let echo_endpoint = FakeTokenEndpoint::spawn("200 OK", echo_body);
    let flow = BaiduOAuthFlow::new(
        ClientCredentials::new(FAKE_CLIENT_ID, FAKE_CLIENT_SECRET).expect("credentials valid"),
    )
    .with_token_endpoint(echo_endpoint.url())
    .expect("loopback fixture endpoint accepted");
    let exchange_err = flow
        .exchange_code(FAKE_CODE, &state)
        .expect_err("2xx error body must reject");
    drop(echo_endpoint);
    assert_eq!(exchange_err.severity, Severity::Fatal);
    assert!(matches!(
        kind_of(&exchange_err),
        Some(OAuthErrorKind::ExchangeResponseInvalid(_))
    ));
    let exchange_display = format!("{exchange_err}");
    assert!(
        !exchange_display.contains(FAKE_CODE) && !exchange_display.contains(FAKE_CLIENT_SECRET),
        "换码错误 Display 零 code/secret 明文: {exchange_display}"
    );
    assert!(
        exchange_display.contains("invalid_grant"),
        "非凭据根因保留: {exchange_display}"
    );
    assert!(
        exchange_display.contains("<redacted>"),
        "生产 redact 缝可见生效: {exchange_display}"
    );

    // ── 阶段 3:123 WebDAV 载体(端点/账号/应用专用密码 → RemoteCreate)──
    let remote = partiverse_core::pan123::webdav_remote(
        "pv-wp04t04",
        FAKE_ENDPOINT,
        FAKE_ACCOUNT,
        FAKE_APP_PASS,
    )
    .expect("remote descriptor builds");
    let remote_debug = format!("{remote:?}");
    for value in [FAKE_ENDPOINT, FAKE_ACCOUNT, FAKE_APP_PASS] {
        assert!(
            !remote_debug.contains(value),
            "RemoteCreate Debug 零载体值明文: {remote_debug}"
        );
    }

    // ── 阶段 4:config/create 提交,rc 回显入参 → 全参数 redact 兜底 ──
    let submit_err =
        schema::create_remote(&FakeEchoDispatch, &remote).expect_err("echo fake rejects");
    assert_eq!(submit_err.severity, Severity::Fatal);
    let rejection = rejection_of(&submit_err).expect("structured rejection payload");
    let rejection_display = format!("{rejection}");
    for value in [FAKE_ENDPOINT, FAKE_ACCOUNT, FAKE_APP_PASS] {
        assert!(
            !rejection_display.contains(value),
            "create 拒绝载荷零入参明文: {rejection_display}"
        );
    }
    assert!(
        rejection_display.contains("<redacted>"),
        "redact 兜底可见生效: {rejection_display}"
    );

    // ── 阶段 5:全链路诊断流经部署过滤器(redact)→ 字节级零明文 ──
    // 阳性基线:六类值注入基线段(证明过滤器非空洞),随后必须全部滤除。
    let mut stream = format!(
        "baseline: {master} {FAKE_CLIENT_SECRET} {FAKE_CODE} {FAKE_ACCESS} {FAKE_REFRESH} {FAKE_APP_PASS}\n\
         chain: {credentials_debug} | {token_debug} | {exchange_display} | {remote_debug} | {rejection_display}"
    );
    for value in [
        master.as_str(),
        FAKE_CLIENT_SECRET,
        FAKE_CODE,
        FAKE_ACCESS,
        FAKE_REFRESH,
        FAKE_APP_PASS,
    ] {
        assert!(
            stream.contains(value),
            "阳性对照失败: {value} 未入原始流,过滤器断言空洞"
        );
        stream = credential_store::redact(&stream, value);
        assert!(!stream.contains(value), "全链路诊断流残留明文: {stream}");
    }
    assert!(
        stream.matches("<redacted>").count() >= 6,
        "过滤器替换痕迹不足,疑未真正生效: {stream}"
    );
}
