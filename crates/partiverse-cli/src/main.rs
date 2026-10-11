//! Partiverse 命令行入口(M1-WP07-T01):手写子命令解析(partisync-cli
//! main.rs 形态先例,零 clap/tokio),错误经 error_chain 出 stderr + 退出码
//! 0/1(engine-install bin 先例)。CLI 零业务逻辑,全部透传 crates。
//!
//! 按次启动形态(任务卡显式声明):每个引擎类子命令在一次调用内
//! `ensure → 执行 → shutdown on exit`;RcdLaunch Drop 兜底 kill——
//! CLI 退出 = 引擎退出 = 运行中 job 终止。

use std::collections::BTreeMap;
use std::fmt;
use std::process::ExitCode;

use partiverse_engine::error::error_chain;

use partiverse_cli::browse;
use partiverse_cli::connection;
use partiverse_cli::engine;
use partiverse_cli::session::Session;

/// 用法文本(CLI 面,英文;零示例凭据)。
const USAGE: &str = "partiverse-cli

usage:
  partiverse-cli engine install|status|ensure|probe|shutdown
  partiverse-cli connection create-local <name> --path <path>
  partiverse-cli connection create-webdav <name> --url <url> --user <user> --pass <pass>
  partiverse-cli connection list
  partiverse-cli connection delete <name>
  partiverse-cli ls <remote:path>

notes:
  engine-scoped commands run a per-invocation engine session (ensure at start,
  graceful shutdown at exit; engine lifetime = process lifetime).";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(output) => {
            if !output.is_empty() {
                println!("{output}");
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: {}", error_chain(&*err));
            ExitCode::FAILURE
        }
    }
}

/// 子命令分派(解析失败/未知命令 → 用法错误;退出码 0/1,卡内 ④)。
fn run(args: &[String]) -> Result<String, Box<dyn std::error::Error>> {
    match args.first().map(String::as_str) {
        Some("engine") => engine_command(args.get(1).map(String::as_str)),
        Some("connection") => connection_command(&args[1..]),
        Some("ls") => ls_command(&args[1..]),
        _ => Err(Box::new(UsageError::new(USAGE))),
    }
}

/// engine 子命令面(install/status 不拉引擎;ensure/probe/shutdown 按次启动)。
fn engine_command(sub: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
    match sub {
        Some("install") => Ok(engine::install()?),
        Some("status") => Ok(engine::status()?),
        Some("ensure") => Ok(engine::ensure()?),
        Some("probe") => Ok(engine::probe()?),
        Some("shutdown") => Ok(engine::shutdown()?),
        _ => Err(Box::new(UsageError::new(USAGE))),
    }
}

/// connection 子命令面(按次启动会话;透传 core 注入通道,CLI 零业务逻辑)。
fn connection_command(args: &[String]) -> Result<String, Box<dyn std::error::Error>> {
    let parsed = parse_args(args)?;
    with_session(|session| connection_body(session, &parsed))
}

/// [`connection_command`] 的命令体(会话已就绪;成功路径显式优雅退出)。
fn connection_body(
    session: &Session,
    parsed: &Parsed,
) -> Result<String, Box<dyn std::error::Error>> {
    let dispatch = session.dispatch();
    match parsed.positional.first().map(String::as_str) {
        Some("create-local") => {
            ensure_shape(parsed, 2, &["--path"])?;
            let name = positional(parsed, 1, "connection create-local <name> --path <path>")?;
            let path = flag(parsed, "--path")?;
            connection::create_local(dispatch, &name, &path)?;
            Ok(format!("connection created: {name} (alias to local path)"))
        }
        Some("create-webdav") => {
            ensure_shape(parsed, 2, &["--url", "--user", "--pass"])?;
            let name = positional(
                parsed,
                1,
                "connection create-webdav <name> --url --user --pass",
            )?;
            let url = flag(parsed, "--url")?;
            let user = flag(parsed, "--user")?;
            let pass = flag(parsed, "--pass")?;
            // 凭据红线:pass 仅入注入通道,零日志零回显(错误路径 core redact 兜底)。
            connection::create_webdav(dispatch, &name, &url, &user, &pass)?;
            Ok(format!("connection created: {name} (webdav)"))
        }
        Some("list") => {
            ensure_shape(parsed, 1, &[])?;
            Ok(connection::list(dispatch)?)
        }
        Some("delete") => {
            ensure_shape(parsed, 2, &[])?;
            let name = positional(parsed, 1, "connection delete <name>")?;
            connection::delete(dispatch, &name)?;
            Ok(format!("connection deleted: {name}"))
        }
        _ => Err(Box::new(UsageError::new(USAGE))),
    }
}

/// `ls <remote:path>`(operations/list fs+remote 两键齐传,T09 F2 实锚形状)。
fn ls_command(args: &[String]) -> Result<String, Box<dyn std::error::Error>> {
    let parsed = parse_args(args)?;
    with_session(|session| {
        ensure_shape(&parsed, 1, &[])?;
        let target = positional(&parsed, 0, "ls <remote:path>")?;
        Ok(browse::ls(session.dispatch(), &target)?)
    })
}

/// 按次启动会话包装:成功路径显式优雅退出;命令体失败时 Drop 兜底 kill
/// (清理失败容错面 = `RcdLaunch` Drop 契约,不再覆盖原始根因)。
fn with_session(
    body: impl FnOnce(&Session) -> Result<String, Box<dyn std::error::Error>>,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut session = Session::open()?;
    match body(&session) {
        Ok(output) => {
            session.shutdown()?;
            Ok(output)
        }
        Err(err) => Err(err),
    }
}

// —— 手写参数解析(零 clap;partisync-cli flag_value 形态先例的严格化)——

/// 解析结果:位置参数 + 长旗标取值(旗标必带值;重复旗标显式拒绝,零歧义)。
struct Parsed {
    positional: Vec<String>,
    flags: BTreeMap<String, String>,
}

/// 解析 `--flag value` 与位置参数(混排合法;`--` 前缀即旗标)。
fn parse_args(args: &[String]) -> Result<Parsed, UsageError> {
    let mut positional = Vec::new();
    let mut flags = BTreeMap::new();
    let mut index = 0;
    while index < args.len() {
        let token = &args[index];
        if let Some(flag) = token.strip_prefix("--") {
            let Some(value) = args.get(index + 1) else {
                return Err(UsageError::new(format!(
                    "missing value for --{flag}\n\n{USAGE}"
                )));
            };
            if flags.insert(token.clone(), value.clone()).is_some() {
                return Err(UsageError::new(format!(
                    "duplicate flag {token}\n\n{USAGE}"
                )));
            }
            index += 2;
        } else {
            positional.push(token.clone());
            index += 1;
        }
    }
    Ok(Parsed { positional, flags })
}

/// 取第 `index` 个位置参数(缺失 → 用法错误,零 panic)。
fn positional(parsed: &Parsed, index: usize, form: &str) -> Result<String, UsageError> {
    parsed
        .positional
        .get(index)
        .cloned()
        .ok_or_else(|| UsageError::new(format!("missing argument (expected: {form})\n\n{USAGE}")))
}

/// 必填旗标取值(缺失 → 用法错误;取值零回显于错误消息——可含凭据)。
fn flag(parsed: &Parsed, name: &str) -> Result<String, UsageError> {
    parsed
        .flags
        .get(name)
        .cloned()
        .ok_or_else(|| UsageError::new(format!("missing required flag {name}\n\n{USAGE}")))
}

/// 形状校验:位置参数数量上限 + 旗标全集(未声明旗标显式拒绝,零歧义)。
fn ensure_shape(
    parsed: &Parsed,
    max_positional: usize,
    allowed: &[&str],
) -> Result<(), UsageError> {
    if parsed.positional.len() > max_positional {
        return Err(UsageError::new(format!(
            "unexpected extra arguments\n\n{USAGE}"
        )));
    }
    for flag_name in parsed.flags.keys() {
        if !allowed.contains(&flag_name.as_str()) {
            return Err(UsageError::new(format!(
                "unexpected flag {flag_name}\n\n{USAGE}"
            )));
        }
    }
    Ok(())
}

/// 用法错误(Display 面 = 给定消息;经 error_chain 出 stderr,退出码 1)。
#[derive(Debug)]
struct UsageError(String);

impl UsageError {
    fn new(message: impl Into<String>) -> Self {
        UsageError(message.into())
    }
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// 解析器契约:混排旗标与位置参数;重复旗标/缺值显式拒绝(零 panic 面)。
    #[test]
    fn parse_args_shapes_and_rejections() {
        let args: Vec<String> = ["name", "--path", "/tmp/dir", "extra"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let parsed = parse_args(&args).expect("mixed shape parses");
        assert_eq!(parsed.positional, ["name", "extra"]);
        assert_eq!(
            parsed.flags.get("--path").map(String::as_str),
            Some("/tmp/dir")
        );

        let duplicate: Vec<String> = ["--url", "a", "--url", "b"]
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(parse_args(&duplicate).is_err(), "duplicate flag rejected");

        let dangling: Vec<String> = ["--pass"].iter().map(ToString::to_string).collect();
        assert!(
            parse_args(&dangling).is_err(),
            "flag without value rejected"
        );
    }

    /// 形状校验:越位位置参数与未声明旗标显式拒绝(usage 错误,退出码 1 面)。
    #[test]
    fn ensure_shape_rejects_extra_args_and_unknown_flags() {
        let args: Vec<String> = ["list", "--bogus", "x"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let parsed = parse_args(&args).expect("parses");
        assert!(
            ensure_shape(&parsed, 1, &[]).is_err(),
            "unknown flag rejected"
        );
        let extra: Vec<String> = ["delete", "a", "b"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let parsed = parse_args(&extra).expect("parses");
        assert!(
            ensure_shape(&parsed, 1, &[]).is_err(),
            "extra positional rejected"
        );
    }
}
