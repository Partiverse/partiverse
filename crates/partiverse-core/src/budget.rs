//! 配额预算器骨架(M1-WP03-T03):每 Node 一张自写令牌桶(固定窗口计数,零新依
//! 赖;仅调度**请求频率**配额,字节限速下沉 rclone `core/bwlimit`,见 [`bwlimit`])。
//! 决策面 [`BudgetScheduler::acquire`]→`Allow|Throttled(wait_until)|Exhausted`
//! (Exhausted=cost 超桶容的结构性拒绝;Throttled 可等待,wait_until=窗口尾与
//! 429/503 退避终点较大者);T01 `RcThrottled` 反馈 [`BudgetScheduler::backoff`]
//! 延长桶等待——退避**仅对显式配置 profile 的 Node 生效**(Owner 裁定
//! 2026-10-10:否决「无 profile 同样退避」;无 profile Node 的 429/503 照常
//! 上浮,重试决策交上层,架构 §3「一律交预算器退避」按此收窄)。持久化:budget_state 表 v1
//! 复用 T02 `schema_migrations` 框架与同库文件(v2 迁移只追加),写穿+开桶重载,
//! 重启不丢(架构 §6);时间一律 unix 毫秒(时钟回拨=不滚动,保守)。配置:随仓
//! `config/quota-profiles.default.json`(内置默认,provider 事实带 source 键)+
//! 用户配置目录 `partiverse/quota-profiles.json` 覆盖,禁止硬编码进 .rs。接缝:
//! [`BudgetScheduler::gated_submit`] submit 前置 acquire,Exhausted 经
//! [`JobManager::register_queued`] 留 queued 零引擎触达(复活属 WP06);job 终态
//! 回填计量 [`BudgetScheduler::settle`](done=确认,error=退回预留,V1 骨架策略,
//! Owner 知会项)。时钟经 [`Clock`] 注入,测试用假时钟,零真实 sleep。
//!
//! # 并发约束(Owner 裁定 2026-10-10)
//! 本调度器按**单线程编排消费者**设计:同 Node 的 acquire/settle/persist 交错
//! 调用可产生丢失更新(快照-写库非原子)。WP06 引入并发接线前,须先将
//! [`BudgetScheduler::persist`] 改为持 state 锁贯穿写库。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde_json::Value;

use crate::error::PartisyError;
use crate::jobs::{
    JobErrorKind, JobManager, JobRecord, RcDispatch, db_err, io_err, run_migrations,
};

/// budget_state 表 v1 DDL(挂入 jobs::MIGRATIONS v2;只存桶状态零文件内容,红线 3)。
pub(crate) const BUDGET_STATE_V1: &str = "CREATE TABLE budget_state (
    node TEXT PRIMARY KEY NOT NULL,
    used INTEGER NOT NULL,
    window_start_ms INTEGER NOT NULL,
    backoff_until_ms INTEGER NOT NULL
)";

/// 内置默认档(随仓 JSON 单源,相对本文件 = 仓库根 config/)。
const BUILTIN_JSON: &str = include_str!("../../../config/quota-profiles.default.json");

/// 窗口/退避秒数上限(毫秒化须落在 i64 存储域;真实配置远不可及,防御性校验)。
const SECS_LIMIT: u64 = i64::MAX as u64 / 1_000;

/// 时钟抽象(卡内 ⑥:时序单测注入假时钟,禁真实 sleep)。
pub trait Clock: Send + Sync {
    /// 当前时刻(unix 毫秒;时钟早于 epoch = 结构化 Fatal,零 panic)。
    fn now_millis(&self) -> Result<u64, PartisyError>;
}

/// 真实时钟(生产默认)。
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_millis(&self) -> Result<u64, PartisyError> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .map_err(|err| JobErrorKind::Invalid(format!("clock before epoch: {err}")).fatal())
    }
}

/// 单 provider 配额参数(source 须随文件注明唯一事实来源,本结构只载入不造事实)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileParams {
    pub requests_per_window: u64,
    pub window_secs: u64,
    pub source: String,
}

/// 配额参数表(解析+合并终态):`bwlimit_rate`=rclone `--bwlimit` 同语法全局限速串
/// ("off"=不限),经 [`bwlimit`] 下沉;`backoff`=429/503 反馈退避基数;两者均为
/// 策略参数而非 provider 事实,数值只出自配置文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaProfiles {
    pub profiles: BTreeMap<String, ProfileParams>,
    pub bwlimit_rate: String,
    pub backoff: Duration,
}

impl QuotaProfiles {
    /// 内置默认档(随仓 JSON)。
    pub fn builtin() -> Result<Self, PartisyError> {
        Self::parse(BUILTIN_JSON)
    }

    /// 内置默认 + 显式用户覆盖文件(缺失/不可读 = Fatal,显式路径由调用方负责)。
    pub fn load_with_override(path: &Path) -> Result<Self, PartisyError> {
        let text = std::fs::read_to_string(path).map_err(io_err)?;
        Self::parse_with_base(&text, Some(&Self::builtin()?))
    }

    /// 生产入口:内置默认 + 用户配置目录 `partiverse/quota-profiles.json` 覆盖
    /// (覆盖文件不存在 = 纯内置默认,属设计态而非降级;配置目录不可解析 = Fatal)。
    pub fn load_default() -> Result<Self, PartisyError> {
        let base = Self::builtin()?;
        let Some(dir) = platform_config_dir() else {
            return Err(JobErrorKind::Invalid(
                "platform config dir unresolvable; cannot locate quota-profiles.json".into(),
            )
            .fatal());
        };
        let path = dir.join("partiverse").join("quota-profiles.json");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(base),
            Err(err) => return Err(io_err(err)),
        };
        Self::parse_with_base(&text, Some(&base))
    }

    /// 独立解析(无基座;必填键齐全,否则 Fatal)。
    pub(crate) fn parse(json: &str) -> Result<Self, PartisyError> {
        Self::parse_with_base(json, None)
    }

    /// 解析并以 base 合并:`profiles` 按键覆盖/新增,`bwlimit.rate`/`backoff_secs`
    /// 提供即整键覆盖,未提供继承 base;最终缺失 = 配置损坏 Fatal(零代码默认值)。
    /// 未知键忽略(前向兼容),version 必须 = 1。
    pub(crate) fn parse_with_base(json: &str, base: Option<&Self>) -> Result<Self, PartisyError> {
        const BAD: &str = "quota-profiles";
        let root = serde_json::from_str::<Value>(json)
            .map_err(|err| JobErrorKind::Invalid(format!("{BAD}: non-JSON: {err}")).fatal())?;
        let obj = root
            .as_object()
            .ok_or_else(|| fatal_bad(BAD, "root must be a JSON object"))?;
        if obj.get("version").and_then(Value::as_u64) != Some(1) {
            return Err(fatal_bad(BAD, "version must be integer 1"));
        }
        let mut profiles = base.map(|b| b.profiles.clone()).unwrap_or_default();
        if let Some(raw) = obj.get("profiles") {
            let entries = raw
                .as_object()
                .ok_or_else(|| fatal_bad(BAD, "profiles must be an object"))?;
            for (name, entry) in entries {
                let bad = "profile entry needs positive ints and non-empty source";
                let params = parse_profile(name, entry).ok_or_else(|| fatal_bad(BAD, bad))?;
                profiles.insert(name.clone(), params);
            }
        }
        let inherit = |key: &str| {
            base.ok_or_else(|| fatal_bad(BAD, &format!("{key} missing (no base to inherit)")))
        };
        let bwlimit_rate = match obj.get("bwlimit") {
            Some(raw) => raw
                .get("rate")
                .and_then(Value::as_str)
                .filter(|rate| !rate.trim().is_empty())
                .ok_or_else(|| fatal_bad(BAD, "bwlimit.rate must be a non-empty string"))?
                .to_owned(),
            None => inherit("bwlimit.rate")?.bwlimit_rate.clone(),
        };
        let backoff_secs = match obj.get("backoff_secs") {
            Some(raw) => raw
                .as_u64()
                .filter(|secs| (1..=SECS_LIMIT).contains(secs))
                .ok_or_else(|| fatal_bad(BAD, "backoff_secs must be a positive integer"))?,
            None => inherit("backoff_secs")?.backoff.as_secs(),
        };
        Ok(QuotaProfiles {
            profiles,
            bwlimit_rate,
            backoff: Duration::from_secs(backoff_secs),
        })
    }
}

/// Fatal 配置损坏构造。
fn fatal_bad(context: &str, detail: &str) -> PartisyError {
    JobErrorKind::Invalid(format!("{context}: {detail}")).fatal()
}

/// 单 profile 条目解析(损坏 → None,含正整数校验,毫秒化须落在 i64 域)。
fn parse_profile(name: &str, entry: &Value) -> Option<ProfileParams> {
    let obj = entry.as_object()?;
    let source = obj.get("source")?.as_str()?.trim();
    if source.is_empty() || name.is_empty() {
        return None;
    }
    let positive = |key: &str| {
        obj.get(key)?
            .as_u64()
            .filter(|n| (1..=SECS_LIMIT).contains(n))
    };
    Some(ProfileParams {
        requests_per_window: positive("requests_per_window")?,
        window_secs: positive("window_secs")?,
        source: source.to_owned(),
    })
}

/// 平台配置目录(jobs::platform_data_dir 同语义:linux=XDG_CONFIG_HOME(仅绝对
/// 路径)→ $HOME/.config;macOS/win 各自已知目录;平台中立,零 unix 专属 API)。
fn platform_config_dir() -> Option<PathBuf> {
    fn env_dir(key: &str) -> Option<PathBuf> {
        std::env::var_os(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }
    #[cfg(target_os = "windows")]
    {
        env_dir("LOCALAPPDATA")
    }
    #[cfg(target_os = "macos")]
    {
        env_dir("HOME").map(|home| home.join("Library").join("Application Support"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        match env_dir("XDG_CONFIG_HOME") {
            Some(dir) if dir.is_absolute() => Some(dir),
            _ => env_dir("HOME").map(|home| home.join(".config")),
        }
    }
}

/// acquire 决策(卡内 ②):Allow=已扣减;Throttled=可等待;Exhausted=cost 超桶容。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Throttled { wait_until_ms: u64 },
    Exhausted,
}

/// 单 Node 桶状态(= budget_state 行;余量=cap-used;窗口滚动惰性发生在 acquire)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct NodeState {
    used: u64,
    window_start_ms: u64,
    backoff_until_ms: u64,
}

/// job 终态回填计量(卡内 ⑤):Consumed=done 确认;Refund(n)=error 退回预留。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metering {
    Consumed,
    Refund(u64),
}

/// [`BudgetScheduler::gated_submit`] 的 job 规格(T02 submit 同名参数打包)。
#[derive(Debug, Clone, Copy)]
pub struct JobSpec<'a> {
    pub method: &'a str,
    pub kind: &'a str,
    pub src: &'a str,
    pub dst: &'a str,
    pub params: &'a Value,
}

/// gated_submit 结果:Submitted=入引擎;Throttled=暂缺未落行(可重试);
/// Exhausted=job 留 queued 零引擎触达。
#[derive(Debug)]
pub enum GatedSubmit {
    Submitted(JobRecord),
    Throttled { wait_until_ms: u64 },
    Exhausted(JobRecord),
}

/// 配额预算器:每 Node 一张固定窗口桶(卡内 ①),写穿持久化 + 开桶重载。
pub struct BudgetScheduler {
    conn: Mutex<Connection>,
    state: Mutex<BTreeMap<String, NodeState>>,
    profiles: QuotaProfiles,
    clock: Box<dyn Clock>,
}

impl BudgetScheduler {
    /// 显式路径安放(与 JobManager::open_at 同库文件,复用 T02 迁移框架)。
    pub fn open_at(path: &Path, profiles: QuotaProfiles) -> Result<Self, PartisyError> {
        let conn = Connection::open(path).map_err(|err| db_err("open budget db", err))?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|err| db_err("set busy_timeout", err))?;
        run_migrations(&conn)?;
        let state = Self::load_state(&conn)?;
        Ok(BudgetScheduler {
            conn: Mutex::new(conn),
            state: Mutex::new(state),
            profiles,
            clock: Box::new(SystemClock),
        })
    }

    /// 默认安放:用户数据目录 `partiverse/partiverse.db`(与 JobManager 同库)。
    pub fn open_default(profiles: QuotaProfiles) -> Result<Self, PartisyError> {
        let dir = crate::jobs::platform_data_dir()
            .map(|dir| dir.join("partiverse"))
            .ok_or_else(|| {
                JobErrorKind::Invalid(
                    "platform data dir unresolvable; cannot place partiverse.db".into(),
                )
                .fatal()
            })?;
        std::fs::create_dir_all(&dir).map_err(io_err)?;
        Self::open_at(&dir.join("partiverse.db"), profiles)
    }

    /// 注入时钟(测试假时钟;加载不依赖时钟,构造后替换安全)。
    #[must_use]
    pub fn with_clock(mut self, clock: Box<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// 决策接口(卡内 ②):profile 缺失的 Node 默认不限流(Allow,零计量零落库);
    /// 已武装的 429/503 退避对一切 Node 生效(架构 §3「一律」)。
    pub fn acquire(&self, node: &str, cost: u64) -> Result<Decision, PartisyError> {
        let now = self.clock.now_millis()?;
        let profile = self.profiles.profiles.get(node);
        // 结构性:cost 超桶容 = 任何窗口都装不下,不落库。
        if profile.is_some_and(|p| cost > p.requests_per_window) {
            return Ok(Decision::Exhausted);
        }
        let mut state = self.lock_state()?;
        let entry = state.entry(node.to_owned()).or_default();
        let Some(p) = profile else {
            // 无 profile:默认不限流;仅退避武装期阻塞。
            let end = entry.backoff_until_ms;
            drop(state);
            return Ok(if now < end {
                Decision::Throttled { wait_until_ms: end }
            } else {
                Decision::Allow
            });
        };
        let window_ms = p.window_secs.saturating_mul(1_000);
        if now.saturating_sub(entry.window_start_ms) >= window_ms {
            entry.window_start_ms = now;
            entry.used = 0;
        }
        let window_end = entry.window_start_ms.saturating_add(window_ms);
        let backoff_active = now < entry.backoff_until_ms;
        if !backoff_active && entry.used + cost <= p.requests_per_window {
            entry.used += cost;
            drop(state);
            self.persist(node)?;
            return Ok(Decision::Allow);
        }
        let wait_until_ms = window_end.max(entry.backoff_until_ms);
        drop(state);
        Ok(Decision::Throttled { wait_until_ms })
    }

    /// 429/503 反馈退避(卡内 ②):终点单调延长 = max(旧终点, now+基数),写穿
    /// 持久化;重复反馈(连续限流)持续后移。**仅对有 profile 的 Node 生效**
    /// (Owner 裁定 2026-10-10:无 profile Node 空操作,429/503 照常上浮)。
    pub fn backoff(&self, node: &str) -> Result<(), PartisyError> {
        if !self.profiles.profiles.contains_key(node) {
            return Ok(()); // 无 profile:不建行、不退避、不写库
        }
        let now = self.clock.now_millis()?;
        let until = now.saturating_add(self.profiles.backoff.as_millis() as u64);
        {
            let mut state = self.lock_state()?;
            let entry = state.entry(node.to_owned()).or_default();
            entry.backoff_until_ms = entry.backoff_until_ms.max(until);
        }
        self.persist(node)
    }

    /// job 终态回填计量(卡内 ⑤):见 [`Metering`];无 profile 未计量,空操作。
    pub fn settle(&self, node: &str, metering: Metering) -> Result<(), PartisyError> {
        let Metering::Refund(cost) = metering else {
            return Ok(()); // Consumed:确认消费,计数不回退
        };
        if !self.profiles.profiles.contains_key(node) {
            return Ok(());
        }
        {
            let mut state = self.lock_state()?;
            if let Some(entry) = state.get_mut(node) {
                entry.used = entry.used.saturating_sub(cost);
            }
        }
        self.persist(node)
    }

    /// 与 JobManager 接缝(卡内 ⑤):submit 前置 acquire——Allow 即扣减并提交引擎
    /// (submit 随后失败 = 预算已耗,保守方向不放大配额);Throttled 不落行由调用方
    /// 择时重试;Exhausted 经 `register_queued` 留 queued 零引擎触达(复活属 WP06)。
    pub fn gated_submit(
        &self,
        manager: &JobManager,
        dispatch: &impl RcDispatch,
        node: &str,
        cost: u64,
        spec: JobSpec<'_>,
    ) -> Result<GatedSubmit, PartisyError> {
        match self.acquire(node, cost)? {
            Decision::Allow => {
                let job = manager.submit(
                    dispatch,
                    spec.method,
                    spec.kind,
                    spec.src,
                    spec.dst,
                    spec.params,
                )?;
                Ok(GatedSubmit::Submitted(job))
            }
            Decision::Throttled { wait_until_ms } => Ok(GatedSubmit::Throttled { wait_until_ms }),
            Decision::Exhausted => Ok(GatedSubmit::Exhausted(
                manager.register_queued(spec.kind, spec.src, spec.dst)?,
            )),
        }
    }

    /// 桶状态锁(中毒 = 前次持锁 panic,Fatal 上浮,零静默;jobs 同款)。
    fn lock_state(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, BTreeMap<String, NodeState>>, PartisyError> {
        self.state.lock().map_err(|poisoned| {
            JobErrorKind::Db(format!("budget state mutex poisoned: {poisoned}")).fatal()
        })
    }

    /// 连接锁(jobs 同款)。
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, PartisyError> {
        self.conn.lock().map_err(|poisoned| {
            JobErrorKind::Db(format!("budget store mutex poisoned: {poisoned}")).fatal()
        })
    }

    /// 写穿:快照内存态 → budget_state upsert(行先于写消失 = 编程错误,Fatal)。
    /// 单线程约束见模块注释(快照-写库非原子,并发交错可丢失更新;WP06 前改持锁贯穿)。
    fn persist(&self, node: &str) -> Result<(), PartisyError> {
        let (used, window, backoff) = {
            let state = self.lock_state()?;
            let entry = state.get(node).ok_or_else(|| {
                JobErrorKind::Invalid(format!("budget state vanished for `{node}`")).fatal()
            })?;
            (
                entry.used as i64,
                entry.window_start_ms as i64,
                entry.backoff_until_ms as i64,
            )
        };
        self.lock()?
            .execute(
                "INSERT INTO budget_state (node, used, window_start_ms, backoff_until_ms)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(node) DO UPDATE SET used = ?2, window_start_ms = ?3,
                 backoff_until_ms = ?4",
                rusqlite::params![node, used, window, backoff],
            )
            .map_err(|err| db_err("upsert budget_state", err))?;
        Ok(())
    }

    /// 开桶重载(重启不丢;列损坏 = 数据损坏 Fatal,零猜测)。
    fn load_state(conn: &Connection) -> Result<BTreeMap<String, NodeState>, PartisyError> {
        let mut stmt = conn
            .prepare("SELECT node, used, window_start_ms, backoff_until_ms FROM budget_state")
            .map_err(|err| db_err("prepare budget_state", err))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    NodeState {
                        used: row.get::<_, i64>(1)? as u64,
                        window_start_ms: row.get::<_, i64>(2)? as u64,
                        backoff_until_ms: row.get::<_, i64>(3)? as u64,
                    },
                ))
            })
            .map_err(|err| db_err("query budget_state", err))?;
        rows.collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(|err| db_err("collect budget_state", err))
    }
}

/// 字节限速下沉(卡内 ③):经 T01 白名单 `core/bwlimit` 下发全局限速(速率语法 =
/// rclone `--bwlimit`,本机 1.75.1 实测:`1Mi`→1048576;`off`→-1)。`rate=None` = 仅
/// 查询当前态;响应回显 `rate`/`bytesPerSecond`,缺失 = 协议损坏 Fatal(生效断言
/// 由卡内 ⑥ 集成测试对真实引擎执行)。
pub fn bwlimit(
    dispatch: &impl RcDispatch,
    rate: Option<&str>,
) -> Result<BwlimitApplied, PartisyError> {
    let params = match rate {
        Some(rate) => serde_json::json!({ "rate": rate }),
        None => serde_json::json!({}),
    };
    let reply = dispatch.call("core/bwlimit", &params)?;
    let echo = reply
        .get("rate")
        .and_then(Value::as_str)
        .filter(|r| !r.is_empty());
    let bps = reply.get("bytesPerSecond").and_then(Value::as_i64);
    match (echo, bps) {
        (Some(rate), Some(bps)) => Ok(BwlimitApplied {
            rate: rate.to_owned(),
            bytes_per_second: bps,
        }),
        _ => Err(invalid_payload("core/bwlimit", &reply)),
    }
}

/// 下发/查询结果(回显自引擎)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BwlimitApplied {
    pub rate: String,
    pub bytes_per_second: i64,
}

/// rc 载荷异常(Fatal;detail 附原始载荷截断,jobs 同口径)。
fn invalid_payload(method: &str, reply: &Value) -> PartisyError {
    let brief: String = reply.to_string().chars().take(200).collect();
    JobErrorKind::Invalid(format!("{method}: unexpected payload: {brief}")).fatal()
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::jobs::{JobStatus, kind_of};
    use serde_json::json;

    /// 测试档(cap=3,窗口=1000s,退避=60s;fixture 数值非 provider 事实)。
    const TEST_PROFILES: &str = r#"{"version":1,"profiles":{"testprov":{"requests_per_window":3,"window_secs":1000,"source":"test fixture"}},"bwlimit":{"rate":"off","source":"fixture"},"backoff_secs":60}"#;
    const T0: u64 = 1_800_000_000_000;
    const WINDOW_MS: u64 = 1_000_000; // 1000s
    const BACKOFF_MS: u64 = 60_000;

    type Handle = std::sync::Arc<AtomicU64>;

    /// Fake 时钟(仅测试;毫秒域手工推进,共享句柄供测试推进,零真实 sleep)。
    struct FakeClock(Handle);

    impl FakeClock {
        fn at(millis: u64) -> (Self, Handle) {
            let cell: Handle = std::sync::Arc::new(AtomicU64::new(millis));
            (FakeClock(std::sync::Arc::clone(&cell)), cell)
        }
    }

    impl Clock for FakeClock {
        fn now_millis(&self) -> Result<u64, PartisyError> {
            Ok(self.0.load(Ordering::Relaxed))
        }
    }

    /// Fake rc 分发(仅测试;计数+记参+定值应答)。
    struct FakeDispatch {
        calls: Cell<u32>,
        last_params: RefCell<Option<Value>>,
        reply: Value,
    }

    impl FakeDispatch {
        fn replying(reply: Value) -> Self {
            FakeDispatch {
                calls: Cell::new(0),
                last_params: RefCell::new(None),
                reply,
            }
        }
    }

    impl RcDispatch for FakeDispatch {
        fn call(&self, _method: &str, params: &Value) -> Result<Value, PartisyError> {
            self.calls.set(self.calls.get() + 1);
            *self.last_params.borrow_mut() = Some(params.clone());
            Ok(self.reply.clone())
        }
    }

    /// 临时文件库(jobs+budget 同库,联迁证明;进程内唯一名)。
    fn temp_db(tag: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "partiverse-budget-{tag}-{}-{}.db",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn test_profiles() -> QuotaProfiles {
        QuotaProfiles::parse(TEST_PROFILES).expect("fixture parses")
    }

    fn throttled(until: u64) -> Decision {
        Decision::Throttled {
            wait_until_ms: until,
        }
    }

    /// 固定时钟的调度器(文件库;返回时钟句柄供推进)。
    fn scheduler_at(tag: &str, millis: u64) -> (BudgetScheduler, PathBuf, Handle) {
        let path = temp_db(tag);
        let (clock, handle) = FakeClock::at(millis);
        let scheduler = BudgetScheduler::open_at(&path, test_profiles())
            .expect("open scheduler")
            .with_clock(Box::new(clock));
        (scheduler, path, handle)
    }

    #[test]
    fn builtin_carries_sole_provider_fact_with_source() {
        // 卡内 ①:内置档恰一枚 provider 事实(坚果云 600/1800),source 指向架构 §6;
        // 其余 provider 留空(禁止编造);策略参数出自文件(限速 off、退避 60s)。
        let profiles = QuotaProfiles::builtin().unwrap();
        assert_eq!(profiles.profiles.len(), 1);
        let jy = profiles.profiles.get("jianguoyun").unwrap();
        assert_eq!((jy.requests_per_window, jy.window_secs), (600, 1800));
        assert!(
            jy.source.contains("06-架构设计-Phase0草案.md §6"),
            "{}",
            jy.source
        );
        assert_eq!(profiles.bwlimit_rate, "off");
        assert_eq!(profiles.backoff, Duration::from_secs(60));
    }

    #[test]
    fn parse_rejects_corrupt_or_unsourced_config() {
        // 配置损坏一律 Fatal Invalid(kind_of 还原),零代码默认值兜底;profiles 校验
        // 先于 bwlimit/backoff 键,前三例可省略必填键聚焦本例。
        for (label, json) in [
            ("non-object", "[]"),
            ("bad version", r#"{"version":2}"#),
            (
                "missing source",
                r#"{"version":1,"profiles":{"x":{"window_secs":1}}}"#,
            ),
            (
                "zero window",
                r#"{"version":1,"profiles":{"x":{"requests_per_window":1,"window_secs":0,"source":"s"}}}"#,
            ),
            (
                "float cost",
                r#"{"version":1,"profiles":{"x":{"requests_per_window":1.5,"window_secs":1,"source":"s"}}}"#,
            ),
            (
                "empty bwlimit rate",
                r#"{"version":1,"profiles":{},"bwlimit":{"rate":" "}}"#,
            ),
            (
                "missing backoff",
                r#"{"version":1,"profiles":{},"bwlimit":{"rate":"off"}}"#,
            ),
        ] {
            let err = QuotaProfiles::parse(json).expect_err(label);
            assert_eq!(err.severity, crate::error::Severity::Fatal, "{label}");
            assert!(
                matches!(kind_of(&err), Some(JobErrorKind::Invalid(_))),
                "{label}"
            );
        }
    }

    #[test]
    fn override_merges_per_key_and_inherits_rest() {
        // 覆盖语义:profiles 按键覆盖/新增,未提及键继承内置;bwlimit/backoff 提供
        // 即整键覆盖,缺席继承内置。
        let user = r#"{"version":1,"profiles":{"jianguoyun":{"requests_per_window":300,"window_secs":900,"source":"o"},"custom":{"requests_per_window":10,"window_secs":60,"source":"u added"}},"backoff_secs":30}"#;
        let merged =
            QuotaProfiles::parse_with_base(user, Some(&QuotaProfiles::builtin().unwrap())).unwrap();
        let jy = &merged.profiles["jianguoyun"];
        assert_eq!((jy.requests_per_window, jy.window_secs), (300, 900));
        assert_eq!(merged.profiles["custom"].source, "u added");
        assert_eq!(merged.bwlimit_rate, "off", "inherit builtin bwlimit");
        assert_eq!(merged.backoff, Duration::from_secs(30), "override backoff");
    }

    #[test]
    fn window_throttles_exhausts_rolls_over_and_unprofiled_unlimited() {
        // 令牌桶时序(假时钟):2/3 → Allow;再 2 → Throttled(窗口尾);4 > 3 →
        // Exhausted(结构性);过窗 → 重置再 Allow。卡内边界:无 profile 默认不限流。
        let (scheduler, _path, clock) = scheduler_at("window", T0);
        let acq = |node: &str, cost| scheduler.acquire(node, cost).unwrap();
        for _ in 0..3 {
            assert_eq!(acq("local:", 1), Decision::Allow);
        }
        assert_eq!(acq("testprov", 2), Decision::Allow);
        assert_eq!(acq("testprov", 2), throttled(T0 + WINDOW_MS));
        assert_eq!(acq("testprov", 4), Decision::Exhausted);
        clock.fetch_add(WINDOW_MS, Ordering::Relaxed);
        assert_eq!(acq("testprov", 2), Decision::Allow);
    }

    #[test]
    fn backoff_blocks_profiled_and_skips_unprofiled() {
        // 卡内 ②(Owner 裁定 2026-10-10):退避仅武装有 profile 的 Node;无
        // profile 空操作(不建行不退避,acquire 恒 Allow);wait_until=max(窗
        // 尾,退避终点);重复反馈自当前时刻延长。
        let (scheduler, _path, clock) = scheduler_at("backoff", T0);
        let acq = |node: &str, cost| scheduler.acquire(node, cost).unwrap();
        assert_eq!(acq("testprov", 2), Decision::Allow);
        scheduler.backoff("testprov").unwrap();
        scheduler.backoff("local:").unwrap(); // 无 profile:空操作
        assert_eq!(acq("testprov", 1), throttled(T0 + WINDOW_MS));
        assert_eq!(acq("local:", 1), Decision::Allow);
        clock.fetch_add(BACKOFF_MS / 2, Ordering::Relaxed);
        scheduler.backoff("testprov").unwrap();
        assert_eq!(acq("testprov", 1), throttled(T0 + WINDOW_MS));
        clock.fetch_add(BACKOFF_MS, Ordering::Relaxed); // 过退避点,窗口预算仍紧(2/3 用)
        assert_eq!(acq("testprov", 1), Decision::Allow);
        assert_eq!(acq("testprov", 2), throttled(T0 + WINDOW_MS));
    }

    #[test]
    fn persistence_survives_reopen() {
        // 卡内 ④:开桶重载——已计量/退避态经同库文件跨实例存活(重启不丢);
        // 退避行使用有 profile 的 testprov(裁定后无 profile 不写库)。
        let (scheduler, path, _clock) = scheduler_at("persist", T0);
        scheduler.acquire("testprov", 2).unwrap();
        scheduler.backoff("testprov").unwrap();
        drop(scheduler);
        let reloaded = BudgetScheduler::open_at(&path, test_profiles())
            .expect("reopen")
            .with_clock(Box::new(FakeClock::at(T0).0));
        let acq = |node: &str, cost| reloaded.acquire(node, cost).unwrap();
        assert_eq!(acq("testprov", 2), throttled(T0 + WINDOW_MS));
        assert_eq!(acq("local:", 1), Decision::Allow);
    }

    #[test]
    fn gated_submit_and_settle_flow() {
        // 卡内 ⑤:Allow → 入引擎(jobid 锚定);Exhausted → 留 queued 零引擎触达
        // (engine_job_id 空);Throttled → 不落行;终态回填计量——error 退回预留可
        // 再取,done 确认不回退,无 profile 空操作。
        let (scheduler, path, _clock) = scheduler_at("gated", T0);
        let manager = JobManager::open_at(&path).expect("same-db job manager");
        let fake = FakeDispatch::replying(json!({ "jobid": 42 }));
        let params = json!({});
        let spec = JobSpec {
            method: "sync/copy",
            kind: "copy",
            src: "a:",
            dst: "b:",
            params: &params,
        };
        let exhausted = match scheduler
            .gated_submit(&manager, &fake, "testprov", 5, spec)
            .unwrap()
        {
            GatedSubmit::Exhausted(record) => record,
            other => panic!("expected Exhausted, got {other:?}"),
        };
        assert_eq!(fake.calls.get(), 0, "engine untouched on Exhausted");
        assert!(matches!(exhausted.status, JobStatus::Queued));
        assert!(exhausted.engine_job_id.is_none() && exhausted.error.is_none());
        assert_eq!(
            manager.get(&exhausted.id).unwrap().status,
            JobStatus::Queued
        );
        let submitted = match scheduler
            .gated_submit(&manager, &fake, "testprov", 2, spec)
            .unwrap()
        {
            GatedSubmit::Submitted(record) => record,
            other => panic!("expected Submitted, got {other:?}"),
        };
        assert_eq!(submitted.engine_job_id, Some(42));
        assert_eq!(fake.calls.get(), 1);
        let gated = scheduler
            .gated_submit(&manager, &fake, "testprov", 2, spec)
            .unwrap();
        let GatedSubmit::Throttled { wait_until_ms } = gated else {
            panic!("expected Throttled")
        };
        assert_eq!(wait_until_ms, T0 + WINDOW_MS);
        assert_eq!(fake.calls.get(), 1, "engine untouched on Throttled");
        let conn = Connection::open(&path).unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM jobs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 2, "Throttled left no parked row");
        scheduler.settle("testprov", Metering::Refund(2)).unwrap();
        assert_eq!(scheduler.acquire("testprov", 2).unwrap(), Decision::Allow);
        scheduler.settle("testprov", Metering::Consumed).unwrap();
        assert_eq!(scheduler.acquire("testprov", 1).unwrap(), Decision::Allow);
        scheduler.settle("local:", Metering::Refund(99)).unwrap();
    }

    #[test]
    fn bwlimit_maps_echo_and_rejects_broken_payload() {
        // 卡内 ③:参数映射(rate 注入 params;查询模式空参);回显解析;载荷残缺
        // Fatal(本机 1.75.1 实测形状,端到端见集成测试)。
        let fake = FakeDispatch::replying(json!({ "bytesPerSecond": 1_048_576, "rate": "1Mi" }));
        let applied = bwlimit(&fake, Some("1Mi")).unwrap();
        assert_eq!(applied.rate, "1Mi");
        assert_eq!(applied.bytes_per_second, 1_048_576);
        assert_eq!(
            fake.last_params.borrow().as_ref(),
            Some(&json!({ "rate": "1Mi" }))
        );
        assert_eq!(bwlimit(&fake, None).unwrap().rate, "1Mi");
        assert_eq!(fake.last_params.borrow().as_ref(), Some(&json!({})));
        let broken = FakeDispatch::replying(json!({ "rate": "1Mi" }));
        let err = bwlimit(&broken, Some("1Mi")).expect_err("missing bps");
        assert_eq!(err.severity, crate::error::Severity::Fatal);
        assert!(matches!(kind_of(&err), Some(JobErrorKind::Invalid(_))));
    }
}
