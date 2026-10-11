//! pv 运行日志(M1-WP99-T02):每次 `pv` 调用自动追加落盘,Owner dogfood 排障。
//! main 入口唯一包裹(子命令零感知):起始行 = UTC 时间戳 + 脱敏 argv(卡内 ①);
//! 结束行 = 退出码 + 耗时 + 输出摘要首行(stdout 数据不入日志,仅首行防刷爆);
//! 错误路径 = error_chain 全文逐行入日志,stderr 照常(卡内 ②,本模块零打印)。
//!
//! 凭据脱敏(R1 核心,卡内 ③,表驱动):精确旗标表 → 旗标名关键字兜底 →
//! 独立值含关键字整词打码。取舍:宁过掩不欠掩——过掩只损日志可读性,欠掩 =
//! 凭据明文进日志(红线)。静默降级取舍(任务卡裁定,注释声明):日志是辅助
//! 面,路径解析/目录创建/打开/轮转/写入任一失败都不阻塞命令本身——主功能
//! 失败不因日志失败而失败;写失败后本进程内放弃后续日志(避免每行重试刷错)。
//!
//! 路径语义沿 installer `platform_data_dir` 先例(dirs 语义自实现):
//! linux/其他 unix = `XDG_STATE_HOME` → `~/.local/state`;mac 沿 Application
//! Support;windows = `%LOCALAPPDATA%`。落点 `<state>/partiverse/logs/pv.log`。
//! 零第三方依赖、零日志框架(格式 = 单行文本「时间戳 级别 消息」,卡内 ⑥)。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 单日志文件上限(卡内 ⑤):超过即轮转 pv.log → pv.log.1(保留一代,防无限增长)。
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
/// 输出摘要首行字符上限(stdout 数据不入日志的边界面)。
const SUMMARY_MAX_CHARS: usize = 200;
/// 开关环境变量(卡内 ④):`PARTIVERSE_LOG=off` → 完全零写入(测试/脚本静默路径)。
const LOG_ENV: &str = "PARTIVERSE_LOG";
/// 脱敏规则表(卡内 ③):精确旗标名单;独立与 `=值` 两形态,值一律打码。
const SECRET_FLAGS: &[&str] = &["--pass", "--password", "--secret", "--token"];
/// 通用兜底关键字:旗标名(小写化)含关键字 → 值打码;独立值含关键字 → 整词打码。
const SECRET_KEYWORDS: &[&str] = &["pass", "secret", "token"];
/// 脱敏占位符(卡内 ③)。
const MASK: &str = "***";

/// 一次 `pv` 调用的追加式日志记录器(打开失败 = 空记录器,静默降级零阻塞)。
pub struct RunLog {
    file: Option<std::fs::File>,
}

impl RunLog {
    /// 生产入口:env 开关 → 平台 state 路径 → 轮转 → 追加打开(卡内 ④⑤)。
    /// 全程 best-effort,任一步失败 = 空记录器(见模块头取舍声明)。
    pub fn open() -> Self {
        if !gate_enabled(std::env::var(LOG_ENV).ok().as_deref()) {
            return RunLog { file: None };
        }
        match log_file_path() {
            Some(path) => Self::open_at(&path, MAX_LOG_BYTES),
            None => RunLog { file: None },
        }
    }

    /// 注入式打开(单测锚,卡内 ⑥:临时目录注入;`max_bytes` 参数化轮转阈值,
    /// 测试免造 5MB 数据)。生产路径经 [`RunLog::open`] 走真实常量。
    fn open_at(path: &Path, max_bytes: u64) -> Self {
        // 轮转失败不放弃本次日志:文件继续追加(超限容错,静默降级面)。
        let _ = rotate_if_needed(path, max_bytes);
        let opened = path
            .parent()
            .and_then(|parent| fs::create_dir_all(parent).ok())
            .and_then(|()| OpenOptions::new().create(true).append(true).open(path).ok());
        RunLog { file: opened }
    }

    /// 起始行(卡内 ①):argv 以 `Debug` 列表形态入消息(边界无歧义,天然单行)。
    pub fn log_start(&mut self, args: &[String]) {
        self.emit("INFO", format!("start argv={:?}", redact(args)));
    }

    /// 结束行(卡内 ①):退出码 + 耗时 + 输出摘要首行;错误路径传 error_chain
    /// 首行,保证结束行自含可读。
    pub fn log_end(&mut self, exit_code: i32, elapsed: Duration, output: &str) {
        let summary = summarize(output);
        let message = format!(
            "end exit={exit_code} elapsed_ms={} out={summary}",
            elapsed.as_millis()
        );
        self.emit("INFO", message);
    }

    /// 错误记录(卡内 ②):error_chain 全文逐行入日志(stderr 打印归 main,
    /// 本模块零输出,避免同一错误双写两路)。
    pub fn log_error_chain(&mut self, chain_text: &str) {
        for line in chain_text.lines() {
            self.emit("ERROR", line.to_owned());
        }
    }

    /// 单行落盘(格式:`时间戳 级别 消息`)。写失败 = 放弃本进程后续日志
    /// (静默降级,取舍已在模块头声明;主功能零感知零阻塞)。
    fn emit(&mut self, level: &str, message: String) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let timestamp = utc_now_timestamp();
        let line = format!("{timestamp} {level} {}\n", single_line(&message));
        if file.write_all(line.as_bytes()).is_err() {
            self.file = None;
        }
    }
}

/// 脱敏(卡内 ③,表驱动 + 兜底,依序):① 精确旗标表:独立形态(`--pass 值`)
/// 打码下一 token,`=值` 形态打码等号后段;② 旗标名关键字兜底:任一 `--flag`
/// 名含关键字(如 `--webdav-pass`)同上处理——旗标名本体保留,日志仍可读
/// 「传了什么旗标」;③ 独立值兜底(卡内「含 pass/secret/token 字样的值打码」):
/// 非旗标 token 本身含关键字 → 整词打码(位置参数夹带凭据的最后防线)。
pub fn redact(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut mask_next = false;
    for arg in args {
        if mask_next {
            out.push(MASK.to_owned());
            mask_next = false;
            continue;
        }
        // 旗标 token(`--` 前缀)先行:仅 `--flag=value` 形态拆名,普通值里的
        // `=` 不拆;独立值不走旗标分支(关键字兜底在下方,整词自打码)。
        if arg.starts_with("--") {
            let (name, inline_value) = match arg.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (arg.as_str(), None),
            };
            if is_secret_name(name) {
                match inline_value {
                    Some(_) => out.push(format!("{name}={MASK}")),
                    None => {
                        out.push(name.to_owned());
                        mask_next = true;
                    }
                }
                continue;
            }
            out.push(arg.clone());
            continue;
        }
        // 独立值兜底(卡内「含 pass/secret/token 字样的值打码」):整词自打码。
        if contains_keyword(arg) {
            out.push(MASK.to_owned());
            continue;
        }
        out.push(arg.clone());
    }
    out
}

/// 旗标名命中脱敏规则:精确表命中,或小写化后含关键字(关键字判定大小写
/// 不敏感:凭据值大小写混排不可假设)。
fn is_secret_name(name: &str) -> bool {
    SECRET_FLAGS.contains(&name) || contains_keyword(name)
}

fn contains_keyword(text: &str) -> bool {
    let lower = text.to_lowercase();
    SECRET_KEYWORDS
        .iter()
        .any(|keyword| lower.contains(keyword))
}

/// 卡内 ④:仅精确 `off` 关闭(完全零写入);缺省/其余值一律开启——不引入
/// 多档日志级别(卡内未要求,交付最小化)。
fn gate_enabled(value: Option<&str>) -> bool {
    value != Some("off")
}

/// 生产日志路径:`<state>/partiverse/logs/pv.log`(解析失败 = None → 空记录器)。
fn log_file_path() -> Option<PathBuf> {
    state_root_with(&|key| std::env::var(key).ok())
        .map(|root| root.join("partiverse").join("logs").join("pv.log"))
}

/// 平台 state 根目录(env 查找经 `lookup` 注入:单测假 env,零进程环境互扰)。
/// linux/其他 unix:`XDG_STATE_HOME`(空值视同未设,XDG 规范)→ `~/.local/state`;
/// mac:无 state 专目录,沿 `~/Library/Application Support`;
/// windows:`%LOCALAPPDATA%`,缺省回退 `%USERPROFILE%\AppData\Local`。
fn state_root_with(lookup: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(
            PathBuf::from(lookup("HOME")?)
                .join("Library")
                .join("Application Support"),
        )
    }
    #[cfg(windows)]
    {
        lookup("LOCALAPPDATA").map(PathBuf::from).or_else(|| {
            lookup("USERPROFILE").map(|home| PathBuf::from(home).join("AppData").join("Local"))
        })
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(xdg) = lookup("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
            return Some(PathBuf::from(xdg));
        }
        Some(PathBuf::from(lookup("HOME")?).join(".local").join("state"))
    }
}

/// 轮转(卡内 ⑤):`pv.log` 存在且 > `max_bytes` → 改名 `pv.log.1`(已有一代
/// 被覆盖,保留一代)。不存在/未超限/改名失败均不阻断调用方。
fn rotate_if_needed(path: &Path, max_bytes: u64) -> std::io::Result<()> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(()); // 首次运行无文件 = 无需轮转
    };
    if metadata.len() <= max_bytes {
        return Ok(());
    }
    let mut rotated = path.as_os_str().to_owned();
    rotated.push(".1");
    fs::rename(path, PathBuf::from(rotated))
}

/// 输出摘要首行(卡内 ①):取首行,超长按字符边界截断加省略号;空输出 = 空摘要。
fn summarize(text: &str) -> String {
    let first = text.lines().next().unwrap_or_default();
    if first.chars().count() <= SUMMARY_MAX_CHARS {
        return first.to_owned();
    }
    let mut capped: String = first.chars().take(SUMMARY_MAX_CHARS).collect();
    capped.push('…');
    capped
}

/// 单行化(卡内 ⑥ 格式约束):CR/LF 转义为字面量,消息体不再折行。
fn single_line(text: &str) -> String {
    text.replace('\n', "\\n").replace('\r', "\\r")
}

/// 当前时刻 UTC ISO-8601(毫秒精度);时钟早于 epoch(劣化环境)按 epoch 处理。
fn utc_now_timestamp() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    utc_timestamp(elapsed.as_secs(), elapsed.subsec_millis())
}

/// epoch 秒 + 毫秒 → `YYYY-MM-DDTHH:MM:SS.mmmZ`。日期段为 civil-from-days
/// 公式自实现(1970-01-01 = 天数 0;零第三方依赖,单测锚定已知时刻)。
fn utc_timestamp(secs: u64, millis: u32) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = yoe + era * 400 + u64::from(month <= 2);
    let (hh, mm, ss) = (rem / 3_600, rem % 3_600 / 60, rem % 60);
    format!("{year:04}-{month:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}.{millis:03}Z")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    /// 测试 argv 构造助手(&str 切片 → String 向量)。
    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    /// 临时沙箱目录(pid + 进程内序号防并行互撞;收尾尽力清理)。
    fn temp_dir(tag: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pv-t02-{tag}-{}-{seq}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// 卡内 ③ 表驱动:四旗标独立形态 = 值打码、旗标保留;`=值` 形态打码等号后段。
    #[test]
    fn redact_secret_flag_table_both_forms() {
        let separate = redact(&args(&["create-webdav", "demo", "--pass", "hunter2"]));
        assert_eq!(separate, ["create-webdav", "demo", "--pass", MASK]);
        let inline = redact(&args(&["--password=abc", "--secret=s1", "--token=t1"]));
        assert_eq!(inline, ["--password=***", "--secret=***", "--token=***"]);
        assert_eq!(redact(&args(&["ls", "--cost", "3"])), ["ls", "--cost", "3"]);
    }

    /// 卡内 ③ 兜底:旗标名含关键字(大小写不敏感)值打码;独立值整词自打码。
    #[test]
    fn redact_keyword_fallback_flag_and_bare_value() {
        let flagged = redact(&args(&["x", "--webdav-pass", "p/wd", "--Token=abc"]));
        assert_eq!(flagged, ["x", "--webdav-pass", MASK, "--Token=***"]);
        let bare = redact(&args(&["import", "my-Secret-value", "plain"]));
        assert_eq!(bare, ["import", MASK, "plain"], "独立值整词自打码");
        assert_eq!(redact(&args(&["--pass"])), ["--pass"], "悬尾旗标自身保留");
    }

    /// 卡内 ④:仅精确 `off` 关闭;缺省/大小写不同/其余值一律开启(严格值域)。
    #[test]
    fn gate_only_exact_off_disables() {
        assert!(!gate_enabled(Some("off")), "off = 完全零写入");
        assert!(gate_enabled(None), "缺省 = 开启");
        assert!(gate_enabled(Some("OFF")), "非精确值一律开启(严格值域)");
        assert!(gate_enabled(Some("on")));
    }

    /// 卡内 ⑥ + ①:嵌套目录自建、追加单行格式、argv 脱敏零明文、跨打开追加。
    #[test]
    fn open_creates_dirs_appends_single_line_format() {
        let dir = temp_dir("append");
        let path = dir.join("nested").join("logs").join("pv.log");
        let mut log = RunLog::open_at(&path, MAX_LOG_BYTES);
        log.log_start(&args(&["ls", "demo:", "--pass", "hunter2"]));
        drop(log);
        let first = fs::read_to_string(&path).expect("log readable");
        let lines: Vec<&str> = first.lines().collect();
        assert_eq!(lines.len(), 1, "单行格式: {first:?}");
        let timestamp = lines[0].split(' ').next().unwrap_or_default();
        let shaped = timestamp.len() == 24 && timestamp.ends_with('Z') && timestamp.contains('T');
        assert!(shaped, "UTC 时间戳形态: {}", lines[0]);
        assert!(lines[0].contains(" INFO start argv="));
        assert!(lines[0].contains(r#""--pass", "***""#));
        assert!(!lines[0].contains("hunter2"));

        // 二次打开 = 追加,历史行保留(卡内 ①「自动追加」)。
        let mut log = RunLog::open_at(&path, MAX_LOG_BYTES);
        log.log_end(0, Duration::from_millis(42), "hello\nworld");
        drop(log);
        let all = fs::read_to_string(&path).expect("log readable");
        let lines: Vec<&str> = all.lines().collect();
        assert_eq!(lines.len(), 2, "跨打开追加: {all:?}");
        let ended = lines[1].contains(" INFO end exit=0 elapsed_ms=42 out=hello");
        assert!(ended, "结束行形状(摘要仅首行): {:?}", lines[1]);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 卡内 ⑤:超限轮转 pv.log → pv.log.1(保留一代);未超限零轮转。
    #[test]
    fn rotation_moves_to_dot_one_keeps_one_generation() {
        let dir = temp_dir("rotate");
        let path = dir.join("pv.log");
        fs::write(&path, b"0123456789ABCDEF").expect("seed oversized log");
        fs::write(dir.join("pv.log.1"), b"stale-generation").expect("seed stale .1");
        let mut log = RunLog::open_at(&path, 8);
        log.log_end(0, Duration::from_millis(1), "ok");
        drop(log);
        let rotated = fs::read(dir.join("pv.log.1")).expect("rotated generation readable");
        assert_eq!(
            rotated, b"0123456789ABCDEF",
            "超限文件整体轮转为 .1(覆盖旧一代)"
        );
        let fresh = fs::read_to_string(&path).expect("fresh log readable");
        assert_eq!(fresh.lines().count(), 1, "新文件从空追加: {fresh:?}");
        assert!(fresh.ends_with('\n'), "每行独立换行结尾");

        let path2 = dir.join("small.log");
        fs::write(&path2, b"tiny").expect("seed small log");
        let mut log = RunLog::open_at(&path2, 8);
        log.log_end(0, Duration::from_millis(1), "ok");
        drop(log);
        let content = fs::read_to_string(&path2).expect("log readable");
        assert!(content.starts_with("tiny"), "未超限零轮转: {content:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// 卡内 ②:error_chain 全文逐行入日志(每链一行 ERROR);stderr 打印归 main。
    #[test]
    fn error_chain_logged_line_by_line() {
        let dir = temp_dir("error-chain");
        let path = dir.join("pv.log");
        let mut log = RunLog::open_at(&path, MAX_LOG_BYTES);
        log.log_error_chain("top: engine probe failed\ncaused by: connect refused");
        drop(log);
        let content = fs::read_to_string(&path).expect("log readable");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "逐行入日志: {content:?}");
        assert!(lines[0].contains(" ERROR top: engine probe failed"));
        assert!(lines[1].contains(" ERROR caused by: connect refused"));
        let _ = fs::remove_dir_all(&dir);
    }

    /// 时间戳自实现锚定已知时刻(含闰日 1709164800 = 2024-02-29)。
    #[test]
    fn utc_timestamp_known_epochs() {
        assert_eq!(utc_timestamp(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(utc_timestamp(1_234_567_890, 0), "2009-02-13T23:31:30.000Z");
        assert_eq!(utc_timestamp(1_700_000_000, 42), "2023-11-14T22:13:20.042Z");
        assert_eq!(utc_timestamp(1_709_164_800, 0), "2024-02-29T00:00:00.000Z");
    }

    /// 摘要面:多行取首行;超长按字符截断(上限 + 省略号);单行化:CR/LF 转义。
    #[test]
    fn summarize_and_single_line_shapes() {
        assert_eq!(summarize("first\nsecond\nthird"), "first");
        assert_eq!(summarize(""), "");
        let long = "x".repeat(SUMMARY_MAX_CHARS + 50);
        let capped = summarize(&long);
        assert_eq!(capped.chars().count(), SUMMARY_MAX_CHARS + 1);
        assert!(capped.ends_with('…'));
        assert_eq!(single_line("a\nb\rc"), "a\\nb\\rc");
        assert_eq!(single_line("plain"), "plain");
    }

    /// 假 env 查找(注入 [`state_root_with`],零进程环境互扰;缺失键 = None)。
    #[cfg(all(unix, not(target_os = "macos")))]
    fn fake_lookup<'a>(map: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key: &str| {
            map.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_string())
        }
    }

    /// 平台 state 根解析(linux 分支):XDG_STATE_HOME 优先(空值视同未设),
    /// 回退 ~/.local/state;HOME 缺失 = None。
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn state_root_linux_resolution() {
        let m = &[("XDG_STATE_HOME", "/tmp/state"), ("HOME", "/home/u")][..];
        assert_eq!(
            state_root_with(&fake_lookup(m)),
            Some(PathBuf::from("/tmp/state"))
        );
        let home = state_root_with(&fake_lookup(&[("HOME", "/home/u")]));
        assert_eq!(home, Some(PathBuf::from("/home/u/.local/state")));
        let empty = state_root_with(&fake_lookup(&[("XDG_STATE_HOME", ""), ("HOME", "/home/u")]));
        assert_eq!(
            empty,
            Some(PathBuf::from("/home/u/.local/state")),
            "空值视同未设"
        );
        assert!(
            state_root_with(&fake_lookup(&[])).is_none(),
            "HOME 缺失 = 不可解析"
        );
    }
}
