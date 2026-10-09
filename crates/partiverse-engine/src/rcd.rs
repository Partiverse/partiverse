//! rcd sidecar 监督器(M1-WP02-T02):spawn / 探活 / 优雅退出 / 崩溃重启。
//!
//! 安全模式(docs/04 选型):rc 权限等同 shell,必须 unix socket(临时目录随机名)
//! 与加密随机源凭据;认证经 env(`RCLONE_RC_USER`/`RCLONE_RC_PASS`,rclone 1.75.1
//! 官方 `--rc-user`/`--rc-pass` 映射,本机实测核实)传递,argv 与日志零明文 pass;
//! 任何路径不落 TCP/默认端口(卡内禁止行为)。rc 调用走 `rclone rc` CLI 子命令连
//! 同一 socket(卡内边界:不引入 HTTP-over-unix-socket 客户端)。探活=`rc/noop`
//! 连通性 + `core/version` 与随仓 manifest 锁定版本断言。
//! 同步阻塞 API(ADR-0004 惯例):async 集成层调用方放 spawn_blocking。

use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};

use partiverse_core::error::Severity;

use crate::error::{EngineError, EngineErrorKind};
use crate::manifest::EngineManifest;

/// rclone 1.75.1 官方 env 映射之一:`--rc-user` → `RCLONE_RC_USER`(本机
/// `rclone help rcd` + 实测核实);服务端与 `rclone rc` 客户端同读,argv 零明文。
const AUTH_USER_ENV: &str = "RCLONE_RC_USER";
/// rclone 1.75.1 官方 env 映射之二:`--rc-pass` → `RCLONE_RC_PASS`(同上核实)。
const AUTH_PASS_ENV: &str = "RCLONE_RC_PASS";

/// 就绪探活总窗口(本机实测 rclone 冷启动 <1s;CI 留足余量)。
const READY_TIMEOUT: Duration = Duration::from_secs(15);
/// 探活轮询间隔(每次探活 = 一个 `rclone rc` 子进程,间隔留出启动喘息)。
const PROBE_INTERVAL: Duration = Duration::from_millis(100);
/// SIGTERM 优雅退出窗口(docs/04 选型实测口径:2 秒),超时 SIGKILL 兜底。
const GRACEFUL_WINDOW: Duration = Duration::from_secs(2);
/// 优雅退出窗口内的子进程退出轮询间隔。
const SHUTDOWN_POLL: Duration = Duration::from_millis(20);
/// 崩溃重启指数退避表(卡内口径:1s/2s/4s/8s/16s,「退避≤5」=最多 5 次)。
const RESTART_DELAYS: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
];

/// rcd 进程状态机最小集(卡内 ⑤,供 T03 编排)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RcdState {
    /// 已拉起,就绪探活进行中(spawn 内部态/重启再探活期)。
    Starting,
    /// `rc/noop` + `core/version` 探活通过(版本与 manifest 断言一致)。
    Ready,
    /// 优雅退出已启动(SIGTERM 已投递,等待收尾)。
    Stopping,
    /// 进程已退出且 socket 已清理(优雅路径终点)。
    Exited,
    /// 终态:崩溃重启耗尽退避上限(错误已 Fatal 上浮,零静默)。
    Failed,
}

/// 一次拉起的连接要素(socket 路径/随机凭据/子进程)。独立于 [`RcdSupervisor`]
/// (后者实现 Drop):重启路径需要把字段逐个搬入既有实例,Drop 类型禁止搬移。
struct RcdLaunch {
    /// 引擎二进制路径(探活子命令复用同一二进制)。
    binary: PathBuf,
    /// 本次实例的 unix socket 路径(临时目录随机名;重启即换新名)。
    socket_path: PathBuf,
    /// 随机认证用户(CSPRNG hex)。
    user: String,
    /// 随机认证口令(CSPRNG hex)。
    pass: String,
    /// 子进程句柄(收尾后置 None)。
    child: Option<Child>,
}

impl RcdLaunch {
    /// 轮询就绪(卡内 ②):子进程先行退出 → 带状态零静默快速失败;探活失败按
    /// Severity 决策——Fatal(版本不符)立即上浮,Retryable 重试至窗口超时。
    fn wait_until_ready(&mut self, expected_version: &str) -> Result<(), EngineError> {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if let Some(child) = self.child.as_mut()
                && let Some(status) = child.try_wait().map_err(EngineError::from_io)?
            {
                return Err(EngineError::new(
                    EngineErrorKind::RcdNotReady(format!(
                        "process exited during startup: {status}"
                    )),
                    Severity::Retryable,
                ));
            }
            match self.probe_ready(expected_version) {
                Ok(()) => return Ok(()),
                Err(probe_err) => {
                    if probe_err.severity() == Severity::Fatal {
                        return Err(probe_err);
                    }
                    if Instant::now() >= deadline {
                        return Err(EngineError::new(
                            EngineErrorKind::RcdNotReady(format!(
                                "not ready within {READY_TIMEOUT:?}"
                            )),
                            Severity::Retryable,
                        )
                        .with_source(Box::new(probe_err)));
                    }
                    thread::sleep(PROBE_INTERVAL);
                }
            }
        }
    }

    /// 探活就绪(卡内 ②):`rc/noop` 连通性 + `core/version` 版本断言,均经
    /// `rclone rc` CLI 子命令走同一 socket。连通/解析类失败 → Retryable;
    /// 版本与 manifest 不符 → Fatal(重试无意义,装错引擎)。
    fn probe_ready(&self, expected_version: &str) -> Result<(), EngineError> {
        self.run_rc("rc/noop")?;
        let payload = self.run_rc("core/version")?;
        let actual = parse_core_version(&payload).ok_or_else(|| {
            EngineError::new(
                EngineErrorKind::RcdNotReady(format!("unparsable core/version payload: {payload}")),
                Severity::Retryable,
            )
        })?;
        if actual == expected_version {
            Ok(())
        } else {
            Err(EngineError::new(
                EngineErrorKind::RcdVersionMismatch {
                    expected: expected_version.to_owned(),
                    actual,
                },
                Severity::Fatal,
            ))
        }
    }

    /// 执行一次 `rclone rc <method>`(同一 socket + env 认证,argv 零凭据);
    /// 成功返回 stdout 文本,非零退出 → Retryable(stderr/stdout 首行截断入载荷)。
    fn run_rc(&self, method: &str) -> Result<String, EngineError> {
        run_rc_method(
            &self.binary,
            &self.socket_path,
            &self.user,
            &self.pass,
            method,
        )
    }

    /// 失败路径收尾:杀进程(SIGKILL)+ 收尸 + 清 socket(NotFound 属正常)。
    fn force_cleanup(&mut self) -> Result<(), EngineError> {
        if let Some(child) = self.child.as_mut()
            && child.try_wait().map_err(EngineError::from_io)?.is_none()
        {
            child.kill().map_err(EngineError::from_io)?;
            child.wait().map_err(EngineError::from_io)?;
        }
        self.child = None;
        match fs::remove_file(&self.socket_path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                Err(EngineError::from_io(err))
            }
            _ => Ok(()),
        }
    }
}

impl Drop for RcdLaunch {
    fn drop(&mut self) {
        // 兜底防孤儿(正常路径走 shutdown()/force_cleanup(),错误全量上浮;
        // Drop 无错误通道,此处是泄漏保险而非设计内的静默降级)。
        if let Some(child) = self.child.as_mut()
            && child.try_wait().map_or(true, |status| status.is_none())
        {
            // try_wait 出错时按「可能仍存活」处理,SIGKILL 兜底。
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_file(&self.socket_path);
    }
}

/// rcd sidecar 监督器:持有子进程与连接参数。手动实现 [`Debug`] 以保证
/// 凭据脱敏(卡内边界:spawn 参数/日志禁止出现明文 pass)。
pub struct RcdSupervisor {
    /// 引擎二进制路径(探活子命令复用同一二进制)。
    binary: PathBuf,
    /// 期望版本(manifest 锁定版本的 rclone 上报形态,如 "v1.75.1")。
    expected_version: String,
    /// 槽位 cache-dir(T03:崩溃重启按同一路径重注入 `--cache-dir`,Q5)。
    cache_dir: PathBuf,
    /// 本次实例的 unix socket 路径(临时目录下随机名;重启即换新名)。
    socket_path: PathBuf,
    /// 随机认证用户(CSPRNG hex;Debug 脱敏)。
    user: String,
    /// 随机认证口令(CSPRNG hex;Debug 脱敏)。
    pass: String,
    /// 子进程句柄(Exited/失败路径置 None)。
    child: Option<Child>,
    /// 当前状态机状态。
    state: RcdState,
}

impl fmt::Debug for RcdSupervisor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RcdSupervisor")
            .field("binary", &self.binary)
            .field("expected_version", &self.expected_version)
            .field("cache_dir", &self.cache_dir)
            .field("socket_path", &self.socket_path)
            .field("user", &"<redacted>")
            .field("pass", &"<redacted>")
            .field("state", &self.state)
            .field("running", &self.child.is_some())
            .finish()
    }
}

impl RcdSupervisor {
    /// 拉起 rcd 并阻塞至就绪探活通过(卡内 ①②):unix socket(临时目录随机名)
    /// 与随机凭据(env 传递)、最小参数集;`cache_dir` 注入 `--cache-dir`
    /// (T03 槽位参数化,rclone 1.75.1 全局 flag,`help flags`+rcd 烟测实测);
    /// 就绪窗口超时或版本断言失败 → 收尾(杀进程+清 socket)后错误上浮,不留
    /// 孤儿进程。
    pub fn spawn(binary: &Path, cache_dir: &Path) -> Result<Self, EngineError> {
        // 版本断言基准 = 随仓 manifest 锁定版本(rclone 上报形态带 "v" 前缀,
        // 本机 core/version 实测核实)。manifest 非法 → Fatal 原样上浮。
        let expected_version = format!("v{}", EngineManifest::embedded()?.version);
        let mut launched = launch_and_probe(binary, cache_dir, &expected_version)?;
        // RcdLaunch 实现 Drop(泄漏保险),不可整体搬移:mem::take 逐字段提取,
        // 提取后的空壳 Drop 无害(child=None → 跳过收尸;空路径 → NotFound 忽略)。
        Ok(Self {
            binary: binary.to_path_buf(),
            expected_version,
            cache_dir: cache_dir.to_path_buf(),
            socket_path: std::mem::take(&mut launched.socket_path),
            user: std::mem::take(&mut launched.user),
            pass: std::mem::take(&mut launched.pass),
            child: std::mem::take(&mut launched.child),
            state: RcdState::Ready,
        })
    }

    /// 就绪后按需探活:执行一次 `core/version` 并返回上报版本串(如 "v1.75.1");
    /// 连通失败/载荷不可解析经 `RcdNotReady`(Retryable)上浮,零吞错(T03 卡⑥
    /// 集成测试与 WP03 rc 客户端落地前的最小观测面)。
    pub fn probe_core_version(&self) -> Result<String, EngineError> {
        let payload = run_rc_method(
            &self.binary,
            &self.socket_path,
            &self.user,
            &self.pass,
            "core/version",
        )?;
        parse_core_version(&payload).ok_or_else(|| {
            EngineError::new(
                EngineErrorKind::RcdNotReady(format!("unparsable core/version payload: {payload}")),
                Severity::Retryable,
            )
        })
    }

    /// 当前状态(状态机为 T03 编排的观测面)。
    #[must_use]
    pub fn state(&self) -> RcdState {
        self.state
    }

    /// 引擎进程 PID(未启动/已收尾 → `None`;已退出的 PID 可能返 0,一并归 `None`)。
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id).filter(|pid| *pid != 0)
    }

    /// 本次实例的 unix socket 路径(探活子命令与 T03 rc 客户端复用)。
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// 确保引擎进程存活(卡内 ④):检测到意外退出即按指数退避(1s/2s/4s/8s/16s)
    /// 自动重启,每次重启换全新 socket/凭据并重新探活;最多 5 次,全部失败 →
    /// `Failed` 终态 + Fatal `RcdRestartExhausted` 上浮(零静默)。
    pub fn ensure_running(&mut self) -> Result<(), EngineError> {
        match self.state {
            RcdState::Ready | RcdState::Starting => {}
            RcdState::Stopping | RcdState::Exited | RcdState::Failed => {
                return Err(EngineError::new(
                    EngineErrorKind::RcdInvalidState(format!(
                        "ensure_running called in state {:?}",
                        self.state
                    )),
                    Severity::Fatal,
                ));
            }
        }
        let child = self.child.as_mut().ok_or_else(|| {
            EngineError::new(
                EngineErrorKind::RcdInvalidState("child handle missing".into()),
                Severity::Fatal,
            )
        })?;
        let Some(status) = child.try_wait().map_err(EngineError::from_io)? else {
            return Ok(()); // 存活,无事可做
        };
        // —— 意外退出(含被外部 SIGKILL):进入指数退避重启 ——
        let mut last_cause = format!("unexpected exit: {status}");
        self.child = None;
        self.state = RcdState::Starting;
        for delay in RESTART_DELAYS {
            thread::sleep(delay);
            // 重启按同槽位 cache_dir 重注入(槽位参数随监督器存活周期保持)。
            match launch_and_probe(&self.binary, &self.cache_dir, &self.expected_version) {
                Ok(mut launched) => {
                    // mem::take 逐字段提取(RcdLaunch 有 Drop 不可搬移,见 spawn 注)。
                    self.socket_path = std::mem::take(&mut launched.socket_path);
                    self.user = std::mem::take(&mut launched.user);
                    self.pass = std::mem::take(&mut launched.pass);
                    self.child = std::mem::take(&mut launched.child);
                    self.state = RcdState::Ready;
                    return Ok(());
                }
                Err(err) => last_cause = err.to_string(),
            }
        }
        self.state = RcdState::Failed;
        Err(EngineError::new(
            EngineErrorKind::RcdRestartExhausted {
                attempts: RESTART_DELAYS.len() as u32,
                last_cause,
            },
            Severity::Fatal,
        ))
    }

    /// 优雅退出(卡内 ③):SIGTERM → 2s 窗口轮询 → 超时 SIGKILL 兜底;退出后
    /// 清理 socket(NotFound = rclone 收到 SIGTERM 已自清理,属正常)。
    pub fn shutdown(&mut self) -> Result<(), EngineError> {
        match self.state {
            RcdState::Starting | RcdState::Ready => {}
            other => {
                return Err(EngineError::new(
                    EngineErrorKind::RcdInvalidState(format!("shutdown called in state {other:?}")),
                    Severity::Fatal,
                ));
            }
        }
        self.state = RcdState::Stopping;
        let child = self.child.as_mut().ok_or_else(|| {
            EngineError::new(
                EngineErrorKind::RcdInvalidState("child handle missing".into()),
                Severity::Fatal,
            )
        })?;
        if child.try_wait().map_err(EngineError::from_io)?.is_none() {
            #[cfg(unix)]
            {
                use nix::sys::signal::{Signal, kill};
                use nix::unistd::Pid;
                let pid = Pid::from_raw(child.id() as i32);
                if let Err(err) = kill(pid, Signal::SIGTERM)
                    && err != nix::errno::Errno::ESRCH
                {
                    // 投递失败非 ESRCH(权限等):不再盲目重试,显式 Fatal 上浮。
                    return Err(EngineError::new(
                        EngineErrorKind::RcdShutdownFailed(format!(
                            "SIGTERM delivery failed: {err}"
                        )),
                        Severity::Fatal,
                    ));
                    // ESRCH(检查与投递间隙内自行退出)走下方收尾轮询,属正常。
                }
            }
            let deadline = Instant::now() + GRACEFUL_WINDOW;
            loop {
                if child.try_wait().map_err(EngineError::from_io)?.is_some() {
                    break;
                }
                if Instant::now() >= deadline {
                    // 2s 窗口内未优雅退出:SIGKILL 兜底(选型口径)。
                    child.kill().map_err(EngineError::from_io)?;
                    child.wait().map_err(EngineError::from_io)?;
                    break;
                }
                thread::sleep(SHUTDOWN_POLL);
            }
        }
        match fs::remove_file(&self.socket_path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                return Err(EngineError::from_io(err));
            }
            _ => {}
        }
        self.child = None;
        self.state = RcdState::Exited;
        Ok(())
    }
}

impl Drop for RcdSupervisor {
    fn drop(&mut self) {
        // 兜底防孤儿(正常路径走 shutdown(),错误全量上浮;Drop 无错误通道,
        // 此处是泄漏保险而非设计内的静默降级)。
        if let Some(child) = self.child.as_mut()
            && child.try_wait().map_or(true, |status| status.is_none())
        {
            // try_wait 出错时按「可能仍存活」处理,SIGKILL 兜底。
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_file(&self.socket_path);
    }
}

/// 对指定连接要素执行一次 `rclone rc <method>`(同一 socket + env 认证,argv
/// 零凭据);成功返回 stdout 文本,非零退出 → Retryable(stderr/stdout 首行截断
/// 入载荷)。探活路径与 `probe_core_version` 观测面共用同一实现,零分叉。
fn run_rc_method(
    binary: &Path,
    socket_path: &Path,
    user: &str,
    pass: &str,
    method: &str,
) -> Result<String, EngineError> {
    let output = Command::new(binary)
        .arg("rc")
        .arg("--unix-socket")
        .arg(socket_path)
        .env(AUTH_USER_ENV, user)
        .env(AUTH_PASS_ENV, pass)
        .arg(method)
        .output()
        .map_err(|err| {
            EngineError::new(
                EngineErrorKind::RcdNotReady(format!("rc probe spawn failed: {err}")),
                Severity::Retryable,
            )
        })?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    // rclone rc 把错误 JSON 打到 stdout、NOTICE 打到 stderr:都带上,截断防刷屏。
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = first_line(&stderr)
        .or_else(|| first_line(&stdout))
        .unwrap_or_else(|| "no diagnostic output".into());
    Err(EngineError::new(
        EngineErrorKind::RcdNotReady(format!("rc {method} failed ({}): {detail}", output.status)),
        Severity::Retryable,
    ))
}

/// 拉起 + 就绪探活(首次 spawn 与崩溃重启共用同一强度:新 socket/新凭据/全量
/// 探活);失败路径内部收尾(杀进程+清 socket)后上浮,不留孤儿进程。
fn launch_and_probe(
    binary: &Path,
    cache_dir: &Path,
    expected_version: &str,
) -> Result<RcdLaunch, EngineError> {
    let mut launched = launch(binary, cache_dir)?;
    match launched.wait_until_ready(expected_version) {
        Ok(()) => Ok(launched),
        Err(err) => {
            let err = match launched.force_cleanup() {
                Ok(()) => err,
                // 收尾失败(孤儿/残留风险)比启动失败更需行动力:挂为根因链上浮。
                Err(cleanup) => err.with_source(Box::new(cleanup)),
            };
            Err(err)
        }
    }
}

/// 生成连接要素并 spawn rcd(卡内 ①:unix socket 随机名 + 随机凭据 + 最小参数集)。
#[cfg(not(unix))]
fn launch(_binary: &Path, _cache_dir: &Path) -> Result<RcdLaunch, EngineError> {
    // 卡内安全模式=unix socket,无 TCP 回退(禁止行为清单):非 unix 平台
    // 显式 Fatal 上浮,不静默降级。
    Err(EngineError::new(
        EngineErrorKind::RcdStartFailed(format!(
            "rcd unix socket supervision requires a unix platform, got {}",
            std::env::consts::OS
        )),
        Severity::Fatal,
    ))
}

/// unix 实现:spawn rcd 进程(最小参数集,认证走 env,argv 零明文 pass;
/// `--cache-dir` 注入槽位缓存路径,T03 槽位参数化)。
#[cfg(unix)]
fn launch(binary: &Path, cache_dir: &Path) -> Result<RcdLaunch, EngineError> {
    let socket_path = random_socket_path()?;
    let (user, pass) = generate_credentials()?;
    // 最小参数集:rcd 默认仅开 rc 服务;`--rc-addr unix://…` 强制 unix socket
    // (1.75.1 实测核实,零 TCP/默认端口);`--cache-dir` 为 1.75.1 全局 flag
    // (`rclone help flags` + rcd 组合烟测实测)。
    let child = Command::new(binary)
        .arg("rcd")
        .arg("--rc-addr")
        .arg(format!("unix://{}", socket_path.display()))
        .arg("--cache-dir")
        .arg(cache_dir)
        .env(AUTH_USER_ENV, &user)
        .env(AUTH_PASS_ENV, &pass)
        .spawn()
        .map_err(|err| {
            EngineError::new(
                EngineErrorKind::RcdStartFailed(format!("spawn {}: {err}", binary.display())),
                Severity::Fatal,
            )
        })?;
    Ok(RcdLaunch {
        binary: binary.to_path_buf(),
        socket_path,
        user,
        pass,
        child: Some(child),
    })
}

/// 解析 `core/version` 载荷中的 `version` 字段(如 "v1.75.1")。
fn parse_core_version(payload: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct CoreVersion {
        version: String,
    }
    serde_json::from_str::<CoreVersion>(payload)
        .ok()
        .map(|parsed| parsed.version)
}

/// 多行文本首行(截断到 200 字符,防错误载荷刷屏)。
fn first_line(text: &str) -> Option<String> {
    let line = text.lines().next().unwrap_or("").trim();
    (!line.is_empty()).then(|| line.chars().take(200).collect())
}

/// 读取 `len` 字节内核 CSPRNG(卡内 ①:加密随机源)。/dev/urandom 与
/// getrandom(2) 同源;std 文件读取零 unsafe、零新依赖(ADR-0005 随机性节)。
#[cfg(unix)]
fn random_bytes(len: usize) -> Result<Vec<u8>, EngineError> {
    let mut buffer = vec![0u8; len];
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut buffer))
        .map_err(|err| {
            EngineError::new(
                EngineErrorKind::RandomSourceUnavailable(format!("read /dev/urandom: {err}")),
                Severity::Fatal,
            )
        })?;
    Ok(buffer)
}

/// 临时目录下随机 socket 名:`partiverse-rcd-<16 hex>.sock`(卡内 ①)。
/// 崩溃重启换新名,SIGKILL 残留的旧 socket 文件由 OS 临时目录策略回收,
/// 与新 socket 互不相干(不做旧文件清理,避免卫生性失败掩盖功能性错误)。
#[cfg(unix)]
fn random_socket_path() -> Result<PathBuf, EngineError> {
    Ok(std::env::temp_dir().join(format!(
        "partiverse-rcd-{}.sock",
        hex::encode(random_bytes(8)?)
    )))
}

/// 随机认证对(卡内 ①:随机 user/pass):各 16 字节 CSPRNG → 32 hex 字符
/// (128 bit 熵);字符集仅 `[0-9a-f]`,Basic Auth 头与 argv 均安全。
#[cfg(unix)]
fn generate_credentials() -> Result<(String, String), EngineError> {
    let user = hex::encode(random_bytes(16)?);
    let pass = hex::encode(random_bytes(16)?);
    Ok((user, pass))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::EngineErrorKind as Kind;
    use crate::installer::EngineInstaller;

    /// 测试二进制解析环境变量(卡内 ⑥ 第一优先级)。
    const TEST_BIN_ENV: &str = "PARTIVERSE_TEST_RCLONE_BIN";

    /// 测试用 rclone 解析顺序(卡内 ⑥):env `PARTIVERSE_TEST_RCLONE_BIN` >
    /// 默认引擎根(manifest 锁定版本);缺失→显式 panic+安装指引,禁止静默 skip。
    /// 解析读 env(PARTIVERSE_*):持 slots::test_env_lock(T03)防并发写竞态。
    fn test_rclone_binary() -> PathBuf {
        let _guard = crate::slots::test_env_lock::ENV_LOCK
            .lock()
            .expect("env lock");
        if let Some(path) = std::env::var_os(TEST_BIN_ENV).filter(|value| !value.is_empty()) {
            let path = PathBuf::from(path);
            assert!(
                path.is_file(),
                "{TEST_BIN_ENV} points to a missing file: {}",
                path.display()
            );
            return path;
        }
        let install_root = EngineInstaller::resolve_install_root()
            .expect("engine install root must be resolvable");
        let manifest = EngineManifest::embedded().expect("embedded manifest must parse");
        let asset = manifest.current_platform().expect("host platform asset");
        let binary = install_root
            .join(&manifest.version)
            .join(&asset.binary_name);
        assert!(
            binary.is_file(),
            "test rclone binary missing at {}; install it with \
             `cargo run -p partiverse-engine --bin engine-install` \
             or set {TEST_BIN_ENV} to the rclone binary",
            binary.display()
        );
        binary
    }

    /// 测试用槽位 cache 目录(pid 唯一化;测试收尾自行清理)。
    fn test_cache_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("partiverse-rcd-cache-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create test cache dir");
        dir
    }

    #[test]
    fn spawn_probes_ready_then_shuts_down_gracefully_and_cleans_socket() {
        // 卡内 ⑥ 场景一:spawn → 探活(Ready)→ 优雅退出(Exited)+ socket 清理;
        // 并断言凭据脱敏(Debug 面零明文 pass/user)。
        let binary = test_rclone_binary();
        let cache_dir = test_cache_dir("graceful");
        let mut supervisor =
            RcdSupervisor::spawn(&binary, &cache_dir).expect("rcd spawn must reach ready");
        assert_eq!(supervisor.state(), RcdState::Ready);
        assert!(
            supervisor.socket_path().exists(),
            "socket must exist while running"
        );
        let debug_dump = format!("{supervisor:?}");
        assert!(
            !debug_dump.contains(&supervisor.pass),
            "pass leaked via Debug"
        );
        assert!(
            !debug_dump.contains(&supervisor.user),
            "user leaked via Debug"
        );
        supervisor
            .shutdown()
            .expect("graceful shutdown must succeed");
        assert_eq!(supervisor.state(), RcdState::Exited);
        assert!(
            !supervisor.socket_path().exists(),
            "socket must be cleaned after shutdown"
        );
        let _ = fs::remove_dir_all(&cache_dir);
    }

    #[test]
    fn sigkill_triggers_backoff_restart_and_reprobes_ready() {
        // 卡内 ⑥ 场景二:SIGKILL 杀进程 → 指数退避自动重启 → 再探活(Ready)。
        let binary = test_rclone_binary();
        let cache_dir = test_cache_dir("restart");
        let mut supervisor =
            RcdSupervisor::spawn(&binary, &cache_dir).expect("rcd spawn must reach ready");
        let original_pid = supervisor.pid().expect("pid available while running");
        let original_socket = supervisor.socket_path().to_path_buf();
        // 测试模拟崩溃:外部 SIGKILL(生产路径无 kill 调用)。
        #[cfg(unix)]
        {
            use nix::sys::signal::{Signal, kill};
            use nix::unistd::Pid;
            kill(Pid::from_raw(original_pid as i32), Signal::SIGKILL)
                .expect("SIGKILL must be delivered");
        }
        // SIGKILL 生效与内核回收存在间隙:轮询 ensure_running 直到观察到换 pid。
        let mut restarted = false;
        for _ in 0..100 {
            match supervisor.ensure_running() {
                Ok(()) => {
                    if supervisor.pid() != Some(original_pid) {
                        restarted = true;
                        break;
                    }
                }
                Err(err) => panic!("crash restart must succeed, got: {err}"),
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(restarted, "supervisor must restart after SIGKILL");
        assert_eq!(
            supervisor.state(),
            RcdState::Ready,
            "restart must re-probe ready"
        );
        assert_ne!(
            supervisor.socket_path(),
            original_socket,
            "restart must use a fresh random socket"
        );
        supervisor
            .shutdown()
            .expect("graceful shutdown after restart");
        let _ = fs::remove_dir_all(&cache_dir);
    }

    #[test]
    fn bad_binary_path_fails_as_fatal() {
        // 卡内 ⑥ 场景三:坏二进制路径 → Failed 上浮(Fatal,不静默、不 panic)。
        let missing = std::env::temp_dir().join("partiverse-missing-rclone-for-t02");
        let cache_dir = test_cache_dir("bad-binary");
        let err =
            RcdSupervisor::spawn(&missing, &cache_dir).expect_err("bad binary path must fail");
        assert_eq!(err.severity(), Severity::Fatal);
        assert!(matches!(err.kind(), Kind::RcdStartFailed(_)));
        let _ = fs::remove_dir_all(&cache_dir);
    }
}
