//! JobManager(M1-WP03-T02;M1-WP06-T01 队列核心强化):generic 异步 rc job 状态
//! 机+SQLite 落库+启动对账;严格四态 `queued→running→done|error`(非法迁移拒绝并
//! 上浮;done/error 终态零出边)。rc 形状以本机 rclone 1.75.1 实测锚定
//! (`_async`→`{"jobid":<int>}`;`job/status`→`{finished,success,error,output}`;
//! `job/list`→`{jobids:[…]}`;完成 job 默认 60s 过期,过期即判 engine_restarted)。
//!
//! WP06-T01 增量:①submit 原子窗口(先 INSERT queued 无锚→引擎提交→回填
//! engine_job_id,崩溃/提交失败残留 = queued 无锚,经 [`JobManager::revive_parked`]
//! 兜底复活);②poll 两段 UPDATE 事务化(单事务守卫直写);③引擎侧失败分类
//! [`classify_engine_failure`](1.75.1 实机锚定文本表)+Retryable 自动重试回 queued
//! (上限 [`RetryPolicy::max_retries`] 默认 3,超过落 error 终态;Fatal 仅手动);
//! ④queued 复活 [`JobManager::revive_parked`](register_queued 停放行重新提交);
//! ⑤取消 [`JobManager::job_cancel`](job/stop+终态裁决=error/user_canceled/
//! Interrupted,不加第五状态);⑥公开 [`JobManager::list_jobs`](status 过滤);
//! ⑦进度/校验和列写 API(数值来源=core/stats,T02 接线)。
//! 接缝(Cargo 禁普通依赖成环):生产消费面 = [`RcDispatch`] trait(T01 `RcClient`
//! 适配器由引擎/应用层提供,集成测试含适配证明);method 门 = ASYNC_METHODS
//! (适配器 `RcMethod` 二次把关);同步阻塞 API;依赖审计见 ADR-0006;表只存
//! 路径与元数据(红线 3)。

use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use crate::error::{PartisyError, Severity, classify_io};

/// submit 允许集(T01 白名单子集;卡内边界:限 sync/copy|move 等 async 支持命令)。
pub const ASYNC_METHODS: &[&str] = &["sync/copy", "sync/move"];

/// rc 命令分发接缝:生产实现 = T01 `RcClient` 适配(白名单收口+severity 保真)。
pub trait RcDispatch {
    /// 执行一条 rc 命令并返回成功载荷;失败 = severity 保真的 [`PartisyError`]。
    fn call(&self, method: &str, params: &Value) -> Result<Value, PartisyError>;
}

/// job 状态(严格四态;库内 canonical 小写存储;Error=poll 观测 error 或
/// reconcile 判 engine_restarted 的失败终态)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobStatus {
    /// 已登记未推进(submit 初态)。
    Queued,
    /// 引擎侧已确认在跑。
    Running,
    /// 成功终态。
    Done,
    Error,
}

impl JobStatus {
    /// 库内 canonical 字符串。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Done => "done",
            JobStatus::Error => "error",
        }
    }

    /// canonical 字符串解析(非法 → None,调用方按数据损坏上浮)。
    #[must_use]
    pub fn from_str_raw(raw: &str) -> Option<Self> {
        [
            JobStatus::Queued,
            JobStatus::Running,
            JobStatus::Done,
            JobStatus::Error,
        ]
        .into_iter()
        .find(|status| status.as_str() == raw)
    }

    /// 合法迁移表:`queued→running`、`running→done|error`;终态零出边(同态
    /// 幂等不算迁移,见 [`JobManager::advance`])。Retryable 失败的「running→queued
    /// 回队」(卡内 ②)不进本表——它由 [`JobManager::poll`] 的守卫式事务直写落库,
    /// 同时清锚并递增 `retries`,非单纯状态迁移;本表公开契约(WP03 冻结形状,
    /// 含终态语义)保持不变。
    #[must_use]
    pub fn can_transition(self, to: JobStatus) -> bool {
        matches!(
            (self, to),
            (JobStatus::Queued, JobStatus::Running)
                | (JobStatus::Running, JobStatus::Done)
                | (JobStatus::Running, JobStatus::Error)
        )
    }
}

/// 自动重试策略(卡内 ②:Retryable 失败自动回 queued 的上限与退避声明)。
/// `max_retries` 默认 3,可经 [`JobManager::with_retry_policy`] 配置;`backoff`
/// 卡内裁定 = 固定 60s(与 budget 默认退避同基线,quota-profiles `backoff_secs`
/// 内置默认值),由调度编排层(T02 接线)对照行 `updated_at`(回队时刻)执行
/// 等待后调 [`JobManager::revive_parked`],core 零真实 sleep。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Retryable 失败自动回队上限(次,不含首次提交;超过落 error 终态,仅手动)。
    pub max_retries: u32,
    /// 回队后的重试退避间隔(固定值,卡内声明;编排层执行)。
    pub backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            backoff: Duration::from_secs(60),
        }
    }
}

/// 结构化错误类别(severity 机器可判定,零吞错;除 Io 走 classify_io 表驱动
/// 外均 Fatal:非法迁移/本地拒绝/格式损坏/持久化损坏皆重试无意义)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobErrorKind {
    /// 非法迁移(编程错误);载荷=job id 与 from/to。
    IllegalTransition {
        id: String,
        from: JobStatus,
        to: JobStatus,
    },
    /// method 不在 ASYNC_METHODS(本地拒绝,请求未出本进程)。
    MethodRejected(String),
    /// 输入或 rc 载荷不合法(格式损坏)/时钟不可用;载荷=描述。
    Invalid(String),
    /// 文件系统 IO(severity 由核心 `classify_io` 表驱动)。
    Io,
    /// SQLite 层失败(本地持久化损坏)/数据目录不可解析;载荷=描述。
    Db(String),
}

impl fmt::Display for JobErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobErrorKind::IllegalTransition { id, from, to } => {
                write!(
                    f,
                    "illegal job transition `{id}`: {} → {}",
                    from.as_str(),
                    to.as_str()
                )
            }
            JobErrorKind::MethodRejected(method) => {
                write!(f, "rc method not async-capable: {method}")
            }
            JobErrorKind::Invalid(detail) => write!(f, "invalid input or rc payload: {detail}"),
            JobErrorKind::Io => write!(f, "io error"),
            JobErrorKind::Db(detail) => write!(f, "sqlite error: {detail}"),
        }
    }
}

impl std::error::Error for JobErrorKind {}

impl JobErrorKind {
    /// Fatal 构造:kind 挂 `PartisyError.source()`(severity 直读,kind 经 [`kind_of`] 还原;
    /// budget 模块同库复用)。
    pub(crate) fn fatal(self) -> PartisyError {
        PartisyError {
            severity: Severity::Fatal,
            source: Some(Box::new(self)),
        }
    }
}

/// 从 [`PartisyError`] 还原结构化类别(非本模块来源的错误 → None)。
#[must_use]
pub fn kind_of(err: &PartisyError) -> Option<&JobErrorKind> {
    err.source
        .as_ref()
        .and_then(|s| s.downcast_ref::<JobErrorKind>())
}

/// 引擎侧失败文本的 Fatal 签名(小写包含匹配;全部条目 1.75.1 实机观测锚定,
/// 扩充须附实测证据,禁凭记忆)。
const FATAL_MARKERS: &[&str] = &[
    // rc 参数缺失(实测:sync/copy 无 srcFs → `Didn't find key "srcFs" in input`)
    "didn't find key",
    // 源路径不存在(实测:alias→缺失目录 → `directory not found`)
    "directory not found",
    // 目标/源把文件当目录用(实测:http 后端根被当文件 → `is a file not a directory`)
    "is a file not a directory",
    // HTTP 权限(实测:webdav 假源 403/401 → `couldn't list files: 403 Forbidden`
    // / `couldn't list files: 401 Unauthorized`)
    "forbidden",
    "unauthorized",
];

/// 引擎侧失败文本的 Retryable 签名(5xx/429/超时/瞬态网络;全部 1.75.1 实机观测
/// 锚定,扩充须附实测证据,禁凭记忆)。
const RETRYABLE_MARKERS: &[&str] = &[
    // 5xx(实测:webdav 假源 → `couldn't list files: 500 Internal Server Error`
    // / `502 Bad Gateway` / `503 Service Unavailable`)
    "internal server error",
    "bad gateway",
    "service unavailable",
    // 429(实测:`couldn't list files: 429 Too Many Requests`)
    "too many requests",
    // 超时(实测:`... net/http: timeout awaiting response headers`)
    "timeout",
    // 瞬态网络(实测:`... connect: connection refused` / `... read: connection reset
    // by peer`;连接拒绝/重置经 rclone 内部重试后仍可能复现于终态文本)
    "connection refused",
    "connection reset",
];

/// 引擎侧 job 失败分类(卡内 ②,替代 poll 恒 Fatal 现状):按 `job/status` 的
/// `error` 文本性质映射 Severity——权限/参数签名 → Fatal;5xx/429/超时/瞬态网络
/// → Retryable;未识别文本保守按 Fatal(零无限重试,与核心 `classify_io` 未知
/// 默认同口径,扩充文本表须附实机证据)。顺序:Fatal 签名优先(参数/权限属
/// 强特征,命中即终止);`job_cancel` 不经本函数(终态由卡内裁定 Interrupted)。
#[must_use]
pub fn classify_engine_failure(text: &str) -> Severity {
    let lowered = text.to_lowercase();
    if FATAL_MARKERS.iter().any(|marker| lowered.contains(marker)) {
        return Severity::Fatal;
    }
    if RETRYABLE_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
    {
        return Severity::Retryable;
    }
    Severity::Fatal
}

/// IO 错误构造:severity 由核心 `classify_io` 表驱动,原始错误挂根因链(budget 同库复用)。
pub(crate) fn io_err(err: std::io::Error) -> PartisyError {
    PartisyError {
        severity: classify_io(err.kind()),
        source: Some(Box::new(err)),
    }
}

/// SQLite 错误构造(Fatal:本地持久化损坏;原文入 detail;budget 同库复用)。
pub(crate) fn db_err(context: &str, err: rusqlite::Error) -> PartisyError {
    JobErrorKind::Db(format!("{context}: {err}")).fatal()
}

/// jobs 表行(列契约见 `MIGRATIONS` v1/v3 DDL;只存路径字符串与元数据零文件内容,
/// 红线 3)。`id`=本地持久锚(时间戳前缀+随机后缀,引擎重启不失效);
/// `engine_job_id`=运行时锚(rc jobid,引擎重启即失效,由 reconcile 兜底;
/// 回队/submit 窗口内可为 NULL,复活走 [`JobManager::revive_parked`]);
/// `error`/`severity` 仅 error 终态与「queued 待重试」行(reconcile 恒以
/// engine_restarted 起始;取消恒 user_canceled/Interrupted);`progress_bytes`/
/// `progress_total`/`checksum` 来源 = core/stats 与校验和管线(T02 接线,本层只
/// 提供列与写 API);`retries` = Retryable 自动回队累计次数。时间为 RFC3339 UTC 毫秒。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRecord {
    pub id: String,
    pub kind: String,
    pub src: String,
    pub dst: String,
    pub status: JobStatus,
    pub engine_job_id: Option<i64>,
    pub error: Option<String>,
    pub severity: Option<Severity>,
    pub created_at: String,
    pub updated_at: String,
    /// 已传输字节数(core/stats 采样;NULL = 未采样)。
    pub progress_bytes: Option<u64>,
    /// 预估总字节数(同上;NULL = 不可比,按 size+采样降级,T02 裁定)。
    pub progress_total: Option<u64>,
    /// 校验和标注(T02 哈希管线;NULL = 未计算)。
    pub checksum: Option<String>,
    /// Retryable 自动回队累计(上限 [`RetryPolicy::max_retries`])。
    pub retries: u32,
}

/// busy_timeout(卡内强制;单写连接下仅防短暂跨进程争用,保守 5s)。
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// schema_migrations 迁移框架表(v0 基座,先行创建)。
const SCHEMA_MIGRATIONS_DDL: &str = "CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY NOT NULL,
    applied_at TEXT NOT NULL
)";

/// SELECT 列清单单源(get/list 共用;顺序 = [`row_to_record`] 读取序)。
const JOB_COLUMNS: &str = "id, kind, src, dst, status, engine_job_id, error, severity, \
     created_at, updated_at, progress_bytes, progress_total, checksum, retries";

/// 迁移脚本表(下标 i = 版本 i+1;只追加不改写)。v1 = jobs 最小字段集(红线 3);
/// v2 = budget_state(T03 预算桶状态,DDL 单源在 [`crate::budget`],同库同框架);
/// v3 = jobs 增列(WP06-T01,只追加,旧行 NULL 兼容):progress_bytes/progress_total/
/// checksum(卡内 ①)+ retries(卡内 ② 自动重试上限的持久计数,NOT NULL DEFAULT 0
/// ——该列为 ② 的保守必需实现项,超出 ① 字面三列,已在 run 报告与 notes 知会)。
pub(crate) const MIGRATIONS: &[&str] = &[
    "CREATE TABLE jobs (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL,
    src TEXT NOT NULL,
    dst TEXT NOT NULL,
    status TEXT NOT NULL,
    engine_job_id INTEGER,
    error TEXT,
    severity TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
)",
    crate::budget::BUDGET_STATE_V1,
    "ALTER TABLE jobs ADD COLUMN progress_bytes INTEGER;
ALTER TABLE jobs ADD COLUMN progress_total INTEGER;
ALTER TABLE jobs ADD COLUMN checksum TEXT;
ALTER TABLE jobs ADD COLUMN retries INTEGER NOT NULL DEFAULT 0",
];

/// JobManager:状态机+落库+对账(单连接+Mutex:Connection 非 Sync,卡内边界);
/// 重试策略默认 [`RetryPolicy::default`](`with_retry_policy` 可配)。
pub struct JobManager {
    conn: Mutex<Connection>,
    retry: RetryPolicy,
}

impl JobManager {
    /// 默认安放:用户数据目录 `partiverse/partiverse.db`(core 内自实现目录解析)。
    pub fn open_default() -> Result<Self, PartisyError> {
        let dir = platform_data_dir()
            .map(|dir| dir.join("partiverse"))
            .ok_or_else(|| {
                JobErrorKind::Invalid(
                    "platform data dir unresolvable; cannot place partiverse.db".into(),
                )
                .fatal()
            })?;
        std::fs::create_dir_all(&dir).map_err(io_err)?;
        Self::open_at(&dir.join("partiverse.db"))
    }

    /// 显式路径安放(测试与未来显式安置用;同样跑迁移)。
    pub fn open_at(path: &Path) -> Result<Self, PartisyError> {
        let conn = Connection::open(path).map_err(|err| db_err("open db", err))?;
        conn.busy_timeout(BUSY_TIMEOUT)
            .map_err(|err| db_err("set busy_timeout", err))?;
        run_migrations(&conn)?;
        Ok(JobManager {
            conn: Mutex::new(conn),
            retry: RetryPolicy::default(),
        })
    }

    /// 配置自动重试策略(卡内 ② `retries=3 可配`;不设 = 默认 3 次/60s)。
    #[must_use]
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self
    }

    /// 取锁(Mutex 中毒 = 前次持锁 panic,Fatal 上浮,零静默)。
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, PartisyError> {
        self.conn.lock().map_err(|poisoned| {
            JobErrorKind::Db(format!("job store mutex poisoned: {poisoned}")).fatal()
        })
    }

    /// 单行事务骨架(卡内 ⑥ poll 两段 UPDATE 事务化):闭包内重读+守卫更新,
    /// 提交失败回滚后上滚(DB 状态未知,零吞错)。
    fn with_row_tx<T>(
        &self,
        context: &str,
        apply: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, PartisyError>,
    ) -> Result<T, PartisyError> {
        let conn = self.lock()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|err| db_err(context, err))?;
        let out = apply(&tx)?;
        tx.commit()
            .map_err(|err| db_err(&format!("{context} commit"), err))?;
        Ok(out)
    }

    /// 提交 generic 异步 rc job(卡内 ①+⑥ 原子窗口):method/参数门 → 先 INSERT
    /// queued(`engine_job_id` NULL,本地持久锚先行)→ 引擎提交 → UPDATE 回填锚。
    /// 引擎提交失败/进程崩溃残留 = queued 无锚行,由 [`JobManager::revive_parked`]
    /// 兜底复活;白名单外/非 async 命令本地拒绝 Fatal(零残留)。
    pub fn submit(
        &self,
        dispatch: &impl RcDispatch,
        method: &str,
        kind: &str,
        src: &str,
        dst: &str,
        params: &Value,
    ) -> Result<JobRecord, PartisyError> {
        if !ASYNC_METHODS.contains(&method) {
            return Err(JobErrorKind::MethodRejected(method.to_owned()).fatal());
        }
        if kind.is_empty() || src.is_empty() || dst.is_empty() {
            return Err(JobErrorKind::Invalid("kind/src/dst must not be empty".into()).fatal());
        }
        // params 须为 object/null(index_mut 对其他类型 panic,先拦)。
        if !params.is_object() && !params.is_null() {
            return Err(
                JobErrorKind::Invalid(format!("{method}: params must be a JSON object")).fatal(),
            );
        }
        // 原子窗口第一步:queued 无锚行先落库(先持久后触网)。
        let parked = self.register_queued(kind, src, dst)?;
        // 第二步:引擎提交(`_async:true` 注入;失败 = 残留 queued 无锚,可复活)。
        let engine_job_id = async_call(dispatch, method, params)?;
        // 第三步:回填锚(守卫 = queued 且无锚;0 行 = 并发改写,重读实际上浮)。
        let now = now_parts()?.0;
        let changed = self
            .lock()?
            .execute(
                "UPDATE jobs SET engine_job_id = ?1, updated_at = ?2
                 WHERE id = ?3 AND status = 'queued' AND engine_job_id IS NULL",
                rusqlite::params![engine_job_id, now, parked.id],
            )
            .map_err(|err| db_err("backfill engine_job_id", err))?;
        if changed == 0 {
            let actual = self.get(&parked.id)?;
            return Err(JobErrorKind::Invalid(format!(
                "job `{}` left the submit window (status `{}`, engine_job_id {:?})",
                parked.id,
                actual.status.as_str(),
                actual.engine_job_id
            ))
            .fatal());
        }
        Ok(JobRecord {
            engine_job_id: Some(engine_job_id),
            ..parked
        })
    }

    /// 登记一条 queued 行但零引擎触达(M1-WP03-T03 预算接缝:Exhausted 时 job 留
    /// queued 不入引擎,`engine_job_id` 留空;复活经 [`JobManager::revive_parked`]
    /// WP06-T01;也是 [`JobManager::submit`] 原子窗口第一步的落库单源)。
    pub fn register_queued(
        &self,
        kind: &str,
        src: &str,
        dst: &str,
    ) -> Result<JobRecord, PartisyError> {
        if kind.is_empty() || src.is_empty() || dst.is_empty() {
            return Err(JobErrorKind::Invalid("kind/src/dst must not be empty".into()).fatal());
        }
        let (now, now_millis) = now_parts()?;
        let id = new_job_id(now_millis);
        self.lock()?
            .execute(
                "INSERT INTO jobs (id, kind, src, dst, status, engine_job_id, error, severity,
                 created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, NULL, ?6, ?6)",
                rusqlite::params![id, kind, src, dst, JobStatus::Queued.as_str(), now],
            )
            .map_err(|err| db_err("insert parked job", err))?;
        Ok(JobRecord {
            id,
            kind: kind.to_owned(),
            src: src.to_owned(),
            dst: dst.to_owned(),
            status: JobStatus::Queued,
            engine_job_id: None,
            error: None,
            severity: None,
            created_at: now.clone(),
            updated_at: now,
            progress_bytes: None,
            progress_total: None,
            checksum: None,
            retries: 0,
        })
    }

    /// queued 复活(卡内 ③):[`JobManager::register_queued`] 停放的 job(预算
    /// Exhausted 恢复后/Retryable 回队后)重新提交引擎并回填锚。门:method ∈
    /// ASYNC_METHODS、行须 queued 且无锚(带锚 queued/非 queued 行拒绝 Fatal,
    /// 终态零出边);成功即清上次失败标注(`error`/`severity`)开新一轮尝试。
    /// 参数(method/params)不落库,由调用方照首次提交原样供给(行内只有 kind/
    /// src/dst 元数据,红线 3);预算侧 re-acquire 由调用方(编排层)前置。
    pub fn revive_parked(
        &self,
        dispatch: &impl RcDispatch,
        id: &str,
        method: &str,
        params: &Value,
    ) -> Result<JobRecord, PartisyError> {
        if !ASYNC_METHODS.contains(&method) {
            return Err(JobErrorKind::MethodRejected(method.to_owned()).fatal());
        }
        if !params.is_object() && !params.is_null() {
            return Err(
                JobErrorKind::Invalid(format!("{method}: params must be a JSON object")).fatal(),
            );
        }
        let record = self.get(id)?;
        if !matches!(record.status, JobStatus::Queued) || record.engine_job_id.is_some() {
            return Err(JobErrorKind::Invalid(format!(
                "job `{id}` is not revivable: requires status `queued` with no engine anchor \
                 (got status `{}`, anchor {:?})",
                record.status.as_str(),
                record.engine_job_id
            ))
            .fatal());
        }
        let engine_job_id = async_call(dispatch, method, params)?;
        let now = now_parts()?.0;
        let changed = self
            .lock()?
            .execute(
                "UPDATE jobs SET engine_job_id = ?1, error = NULL, severity = NULL, updated_at = ?2
                 WHERE id = ?3 AND status = 'queued' AND engine_job_id IS NULL",
                rusqlite::params![engine_job_id, now, id],
            )
            .map_err(|err| db_err("revive backfill engine_job_id", err))?;
        if changed == 0 {
            let actual = self.get(id)?;
            return Err(JobErrorKind::Invalid(format!(
                "job `{id}` left the revive window (status `{}`, engine_job_id {:?})",
                actual.status.as_str(),
                actual.engine_job_id
            ))
            .fatal());
        }
        self.get(id)
    }

    /// poll(卡内 ②/⑥):done(取 output 作 ret)/error(取文本,按
    /// [`classify_engine_failure`] 分级)→ 事务化落库(单事务守卫直写,替代旧
    /// 两段 UPDATE);Retryable 且 `retries < max` 自动回 queued(清锚,retries+1,
    /// 卡内 ② 自动重试;回队等待由编排层对照 [`RetryPolicy::backoff`] 执行);
    /// Retryable 超限 / Fatal 落 error 终态(Fatal 仅手动)。未 finished → running;
    /// queued 无锚(parked/提交窗口内)零引擎触达原样返回;终态幂等不再触引擎。
    pub fn poll(
        &self,
        dispatch: &impl RcDispatch,
        id: &str,
    ) -> Result<(JobRecord, Option<Value>), PartisyError> {
        let record = self.get(id)?;
        if matches!(record.status, JobStatus::Done | JobStatus::Error) {
            return Ok((record, None));
        }
        // queued 无锚 = 引擎侧无作业可观测(parked 或 submit 原子窗口内),零触达。
        let Some(engine_job_id) = record.engine_job_id else {
            return Ok((record, None));
        };
        let reply = dispatch.call("job/status", &serde_json::json!({ "jobid": engine_job_id }))?;
        let finished = reply
            .get("finished")
            .and_then(Value::as_bool)
            .ok_or_else(|| invalid_payload("job/status", "missing bool `finished`", &reply))?;
        if !finished {
            self.with_row_tx("poll mark-running tx", |tx| {
                let now = now_parts()?.0;
                let changed = tx
                    .execute(
                        "UPDATE jobs SET status = 'running', updated_at = ?1
                         WHERE id = ?2 AND status IN ('queued', 'running')",
                        rusqlite::params![now, id],
                    )
                    .map_err(|err| db_err("poll mark running", err))?;
                if changed == 0 {
                    return settled_elsewhere(tx, id);
                }
                Ok(())
            })?;
            return Ok((self.get(id)?, None));
        }
        let success = reply
            .get("success")
            .and_then(Value::as_bool)
            .ok_or_else(|| invalid_payload("job/status", "missing bool `success`", &reply))?;
        if success {
            self.with_row_tx("poll done tx", |tx| {
                let now = now_parts()?.0;
                let changed = tx
                    .execute(
                        "UPDATE jobs SET status = 'done', error = NULL, severity = NULL,
                         updated_at = ?1 WHERE id = ?2 AND status IN ('queued', 'running')",
                        rusqlite::params![now, id],
                    )
                    .map_err(|err| db_err("poll mark done", err))?;
                if changed == 0 {
                    return settled_elsewhere(tx, id);
                }
                Ok(())
            })?;
            Ok((self.get(id)?, reply.get("output").cloned())) // ret=output(实测)
        } else {
            let error = reply
                .get("error")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .ok_or_else(|| {
                    invalid_payload("job/status", "empty/missing `error` on failed job", &reply)
                })?
                .to_owned();
            let severity = classify_engine_failure(&error);
            self.with_row_tx("poll failure tx", |tx| {
                let now = now_parts()?.0;
                // 事务内重读回队累计(行内最新值定夺重试余量;零吞错)。
                let retries_now: i64 = tx
                    .query_row(
                        "SELECT retries FROM jobs WHERE id = ?1",
                        rusqlite::params![id],
                        |row| row.get(0),
                    )
                    .map_err(|err| db_err("poll read retries", err))?;
                if retries_now < 0 {
                    return Err(JobErrorKind::Invalid(format!(
                        "corrupt job retries: {retries_now}"
                    ))
                    .fatal());
                }
                let changed = if severity == Severity::Fatal {
                    // Fatal 仅手动(卡内 ②):直接终态,severity 保真 fatal。
                    tx.execute(
                        "UPDATE jobs SET status = 'error', error = ?1, severity = 'fatal',
                         updated_at = ?2 WHERE id = ?3 AND status IN ('queued', 'running')",
                        rusqlite::params![error, now, id],
                    )
                    .map_err(|err| db_err("poll mark fatal error", err))?
                } else if retries_now < i64::from(self.retry.max_retries) {
                    // Retryable 自动重试(卡内 ②):回 queued、清锚、累计 retries;
                    // 上次失败文本随行保留(severity=retryable)供 UI/编排层观测。
                    tx.execute(
                        "UPDATE jobs SET status = 'queued', engine_job_id = NULL,
                         retries = retries + 1, error = ?1, severity = 'retryable',
                         updated_at = ?2 WHERE id = ?3 AND status IN ('queued', 'running')",
                        rusqlite::params![error, now, id],
                    )
                    .map_err(|err| db_err("poll requeue retryable", err))?
                } else {
                    // Retryable 超限:error 终态但 severity 保真 retryable(错误性质
                    // 不因重试耗尽而改变;手动复活/新 job 属编排层决策,终态语义不变)。
                    tx.execute(
                        "UPDATE jobs SET status = 'error', error = ?1, severity = 'retryable',
                         updated_at = ?2 WHERE id = ?3 AND status IN ('queued', 'running')",
                        rusqlite::params![error, now, id],
                    )
                    .map_err(|err| db_err("poll mark retryable error", err))?
                };
                if changed == 0 {
                    return settled_elsewhere(tx, id);
                }
                Ok(())
            })?;
            Ok((self.get(id)?, None))
        }
    }

    /// 取消 job(卡内 ④):带锚行先 `job/stop`(1.75.1 实测宽松:运行中/已完成/
    /// 已停均回 `{}`;不存在 = HTTP 500 `job not found`,经 dispatch severity 保真
    /// 上浮,行零改动)→ 终态裁决 = error(cause=`user_canceled`,
    /// Severity::Interrupted;不加第五状态,卡内声明)。终态行取消 = 非法迁移
    /// Fatal(零出边);parked 无锚行零引擎触达直接裁决。
    pub fn job_cancel(
        &self,
        dispatch: &impl RcDispatch,
        id: &str,
    ) -> Result<JobRecord, PartisyError> {
        let record = self.get(id)?;
        if matches!(record.status, JobStatus::Done | JobStatus::Error) {
            return Err(JobErrorKind::IllegalTransition {
                id: id.to_owned(),
                from: record.status,
                to: JobStatus::Error,
            }
            .fatal());
        }
        if let Some(jobid) = record.engine_job_id {
            dispatch.call("job/stop", &serde_json::json!({ "jobid": jobid }))?;
        }
        self.with_row_tx("job cancel tx", |tx| {
            let now = now_parts()?.0;
            let changed = tx
                .execute(
                    "UPDATE jobs SET status = 'error', error = 'user_canceled',
                     severity = 'interrupted', updated_at = ?1
                     WHERE id = ?2 AND status IN ('queued', 'running')",
                    rusqlite::params![now, id],
                )
                .map_err(|err| db_err("job cancel", err))?;
            if changed == 0 {
                // 他方(终态裁决)抢先:幂等让位,返回实际终态,零覆盖。
                return settled_elsewhere(tx, id);
            }
            Ok(())
        })?;
        self.get(id)
    }

    /// 启动对账(卡内 ③,调用方显式触发):running 且 engine_job_id 不在存活
    /// 清单(含行缺锚)→ error(engine_restarted,Fatal;重启后的人工/自动复活属
    /// 编排层决策,本层终态语义不变)。
    pub fn reconcile(&self, dispatch: &impl RcDispatch) -> Result<Vec<JobRecord>, PartisyError> {
        let reply = dispatch.call("job/list", &serde_json::json!({}))?;
        let live_ids = reply
            .get("jobids")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_payload("job/list", "missing array `jobids`", &reply))?
            .iter()
            .map(|value| {
                value.as_i64().ok_or_else(|| {
                    invalid_payload("job/list", "non-integer entry in `jobids`", &reply)
                })
            })
            .collect::<Result<Vec<i64>, PartisyError>>()?;
        let mut reconciled = Vec::new();
        for record in self.list_by_status(JobStatus::Running)? {
            let orphan = match record.engine_job_id {
                Some(jobid) => !live_ids.contains(&jobid),
                None => true,
            };
            if orphan {
                let detail = match record.engine_job_id {
                    Some(jobid) => format!("engine_job_id {jobid} not in live job/list"),
                    None => "job row has no engine_job_id".to_owned(),
                };
                self.advance(
                    &record.id,
                    JobStatus::Running,
                    JobStatus::Error,
                    Some((&format!("engine_restarted: {detail}"), Severity::Fatal)),
                )?;
                reconciled.push(self.get(&record.id)?);
            }
        }
        Ok(reconciled)
    }

    /// 按 job id 取记录(不存在 → Fatal UnknownJob)。
    pub fn get(&self, id: &str) -> Result<JobRecord, PartisyError> {
        self.lock()?
            .query_row(
                &format!("SELECT {JOB_COLUMNS} FROM jobs WHERE id = ?1"),
                rusqlite::params![id],
                row_to_record,
            )
            .optional()
            .map_err(|err| db_err("query job", err))?
            .ok_or_else(|| JobErrorKind::Invalid(format!("unknown job id: {id}")).fatal())
    }

    /// 公开任务清单(卡内 ⑤):`status` 过滤(None = 全量),按 created_at, id 稳定
    /// 排序;UI/编排层的队列视图单源。
    pub fn list_jobs(&self, status: Option<JobStatus>) -> Result<Vec<JobRecord>, PartisyError> {
        match status {
            Some(status) => self.list_by_status(status),
            None => {
                let conn = self.lock()?;
                let mut stmt = conn
                    .prepare(&format!(
                        "SELECT {JOB_COLUMNS} FROM jobs ORDER BY created_at, id"
                    ))
                    .map_err(|err| db_err("prepare list all", err))?;
                let rows = stmt
                    .query_map([], row_to_record)
                    .map_err(|err| db_err("list all jobs", err))?;
                rows.collect::<Result<Vec<JobRecord>, rusqlite::Error>>()
                    .map_err(|err| db_err("collect all jobs", err))
            }
        }
    }

    /// 进度写 API(卡内 ①:列落库+写口;数值来源 = core/stats,T02 接线)。
    /// 行不存在 → Fatal;无状态守卫(stats 采样可能晚于终态,写口保真不丢)。
    pub fn update_progress(
        &self,
        id: &str,
        progress_bytes: u64,
        progress_total: Option<u64>,
    ) -> Result<(), PartisyError> {
        let now = now_parts()?.0;
        let changed = self
            .lock()?
            .execute(
                "UPDATE jobs SET progress_bytes = ?1, progress_total = ?2, updated_at = ?3
                 WHERE id = ?4",
                rusqlite::params![
                    clamp_i64(progress_bytes),
                    progress_total.map(clamp_i64),
                    now,
                    id
                ],
            )
            .map_err(|err| db_err("update progress", err))?;
        if changed == 0 {
            return Err(JobErrorKind::Invalid(format!("unknown job id: {id}")).fatal());
        }
        Ok(())
    }

    /// 校验和写 API(卡内 ①:列落库+写口;内容来源 = T02 哈希管线)。行不存在 → Fatal。
    pub fn set_checksum(&self, id: &str, checksum: &str) -> Result<(), PartisyError> {
        let now = now_parts()?.0;
        let changed = self
            .lock()?
            .execute(
                "UPDATE jobs SET checksum = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![checksum, now, id],
            )
            .map_err(|err| db_err("set checksum", err))?;
        if changed == 0 {
            return Err(JobErrorKind::Invalid(format!("unknown job id: {id}")).fatal());
        }
        Ok(())
    }

    /// 校验失败终态(T03 DoD④:校验失败→job error,Severity 按差异性质——
    /// 内容差异/缺条目 = Fatal,尺寸差异 = Retryable,由 checksum::verify_job
    /// 裁定后传入)。校验发生于 job 完成后,差异是事后才能观测的失败,故本 API
    /// 是**校验通道显式的 done→error 单源**:`can_transition` 冻结表(WP03 形状,
    /// 终态零出边)不变,守卫 `WHERE status='done'` 收口;0 行 = 重读消歧——
    /// 已 error(重复校验失败)幂等返回现行记录(零覆盖),其余状态 = Fatal
    /// (校验只对 done 行有意义)。`error` 列 = 差异细节文本;`checksum` 列不动
    /// (失败不留「通过」标注)。
    pub fn mark_verify_failed(
        &self,
        id: &str,
        error: &str,
        severity: Severity,
    ) -> Result<JobRecord, PartisyError> {
        let now = now_parts()?.0;
        let changed = self
            .lock()?
            .execute(
                "UPDATE jobs SET status = 'error', error = ?1, severity = ?2, updated_at = ?3
                 WHERE id = ?4 AND status = 'done'",
                rusqlite::params![error, severity_raw(severity), now, id],
            )
            .map_err(|err| db_err("mark verify failed", err))?;
        if changed == 0 {
            let actual = self.get(id)?;
            if matches!(actual.status, JobStatus::Error) {
                return Ok(actual);
            }
            return Err(JobErrorKind::Invalid(format!(
                "verification requires a done job: `{id}` is `{}`",
                actual.status.as_str()
            ))
            .fatal());
        }
        self.get(id)
    }

    /// 编排器配置的自动重试策略只读视图(crate 内接缝,M1-WP06-T04):dlink 重取
    /// 编排的余量裁决与 [`JobManager::poll`] 自动重试用同一策略(单一事实源,防
    /// 调用方旁置第二套上限);公开只读面按需另开任务。
    pub(crate) fn retry_policy(&self) -> RetryPolicy {
        self.retry
    }

    /// dlink 重取回队(crate 内专设守卫通道,M1-WP06-T04;先例 = T03
    /// `mark_verify_failed` 校验通道:WP03 冻结 [`JobStatus::can_transition`] 表
    /// 不变,终态出边以窄守卫单源 API 收口)。语义:error 终态 → queued 停放——
    /// 清锚、`retries+1`(重取余量裁决在 `dlink::dlink_refetch_retry` 对照
    /// [`RetryPolicy::max_retries`],本通道只执行计数)、severity 改标 retryable
    /// (dlink 分类裁定的失败性质覆盖通用分类的保守 fatal;错误文本保留供观测,
    /// 与 T01 Retryable 回队同口径)。守卫 `WHERE status='error'`;0 行 = 并发竞速
    /// 重读消歧:已被他方回队(queued 无锚)幂等让位(复活窗口由
    /// [`JobManager::revive_parked`] 守卫仲裁),其余 = 非法迁移 Fatal(零覆盖)。
    pub(crate) fn requeue_for_dlink_refetch(&self, id: &str) -> Result<JobRecord, PartisyError> {
        let now = now_parts()?.0;
        let changed = self
            .lock()?
            .execute(
                "UPDATE jobs SET status = 'queued', engine_job_id = NULL,
                 retries = retries + 1, severity = 'retryable', updated_at = ?1
                 WHERE id = ?2 AND status = 'error'",
                rusqlite::params![now, id],
            )
            .map_err(|err| db_err("requeue for dlink refetch", err))?;
        if changed == 0 {
            let actual = self.get(id)?;
            if matches!(actual.status, JobStatus::Queued) && actual.engine_job_id.is_none() {
                return Ok(actual);
            }
            return Err(JobErrorKind::IllegalTransition {
                id: id.to_owned(),
                from: actual.status,
                to: JobStatus::Queued,
            }
            .fatal());
        }
        self.get(id)
    }

    /// 状态推进单入口:同态幂等;合法迁移直写;queued→终态走合法链两步;非法
    /// 拒绝并上浮。`failure` 仅 error 终态携带(一次落库)。
    fn advance(
        &self,
        id: &str,
        from: JobStatus,
        to: JobStatus,
        failure: Option<(&str, Severity)>,
    ) -> Result<(), PartisyError> {
        if from == to {
            return Ok(());
        }
        if from.can_transition(to) {
            return self.update_status(id, from, to, failure);
        }
        if from == JobStatus::Queued && matches!(to, JobStatus::Done | JobStatus::Error) {
            self.update_status(id, JobStatus::Queued, JobStatus::Running, None)?;
            return self.update_status(id, JobStatus::Running, to, failure);
        }
        Err(JobErrorKind::IllegalTransition {
            id: id.to_owned(),
            from,
            to,
        }
        .fatal())
    }

    /// 守卫式状态更新(WHERE status=from):行数 0 = 并发改写,重读实际状态上浮。
    fn update_status(
        &self,
        id: &str,
        from: JobStatus,
        to: JobStatus,
        failure: Option<(&str, Severity)>,
    ) -> Result<(), PartisyError> {
        let now = now_parts()?.0;
        let changed = {
            let conn = self.lock()?;
            match failure {
                Some((error, severity)) => conn.execute(
                    "UPDATE jobs SET status = ?1, error = ?2, severity = ?3, updated_at = ?4
                     WHERE id = ?5 AND status = ?6",
                    rusqlite::params![
                        to.as_str(),
                        error,
                        severity_raw(severity),
                        now,
                        id,
                        from.as_str()
                    ],
                ),
                None => conn.execute(
                    "UPDATE jobs SET status = ?1, updated_at = ?2 WHERE id = ?3 AND status = ?4",
                    rusqlite::params![to.as_str(), now, id, from.as_str()],
                ),
            }
            .map_err(|err| db_err("update job status", err))?
        };
        if changed == 0 {
            let actual = self.get(id)?;
            return Err(JobErrorKind::IllegalTransition {
                id: id.to_owned(),
                from: actual.status,
                to,
            }
            .fatal());
        }
        Ok(())
    }

    /// 按状态列出记录(list_jobs 过滤面 + reconcile 内部面)。
    fn list_by_status(&self, status: JobStatus) -> Result<Vec<JobRecord>, PartisyError> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {JOB_COLUMNS} FROM jobs WHERE status = ?1 ORDER BY created_at, id"
            ))
            .map_err(|err| db_err("prepare list", err))?;
        let rows = stmt
            .query_map(rusqlite::params![status.as_str()], row_to_record)
            .map_err(|err| db_err("list jobs", err))?;
        rows.collect::<Result<Vec<JobRecord>, rusqlite::Error>>()
            .map_err(|err| db_err("collect jobs", err))
    }
}

/// 异步提交单源(submit 原子窗口第二步 / revive_parked 共用):注入 `_async:true`
/// → dispatch → 校验取整数 `jobid`(实测形状;缺失/非整 = 协议损坏 Fatal)。
fn async_call(
    dispatch: &impl RcDispatch,
    method: &str,
    params: &Value,
) -> Result<i64, PartisyError> {
    let mut payload = params.clone();
    payload["_async"] = Value::Bool(true);
    let reply = dispatch.call(method, &payload)?;
    reply.get("jobid").and_then(Value::as_i64).ok_or_else(|| {
        JobErrorKind::Invalid(format!(
            "{method}: async reply missing integer `jobid`: {reply}"
        ))
        .fatal()
    })
}

/// 守卫更新 0 行的事务内消歧:行已被他方落终态(取消/对账竞速)→ 幂等让位 Ok;
/// 仍处 queued/running = 违规并发改写 → 非法迁移 Fatal;status 损坏 → 数据损坏 Fatal。
fn settled_elsewhere(tx: &rusqlite::Transaction<'_>, id: &str) -> Result<(), PartisyError> {
    let raw: String = tx
        .query_row(
            "SELECT status FROM jobs WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .map_err(|err| db_err("re-read job status", err))?;
    let actual = JobStatus::from_str_raw(&raw)
        .ok_or_else(|| JobErrorKind::Invalid(format!("corrupt job status: {raw}")).fatal())?;
    if matches!(actual, JobStatus::Done | JobStatus::Error) {
        return Ok(());
    }
    Err(JobErrorKind::IllegalTransition {
        id: id.to_owned(),
        from: actual,
        to: JobStatus::Error,
    }
    .fatal())
}

/// 行 → 记录映射(status/severity 列损坏 = 数据损坏,Fatal,零猜测;进度/校验和/
/// retries 负值同样按损坏拒读)。
fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobRecord> {
    let status_raw: String = row.get(4)?;
    let status = JobStatus::from_str_raw(&status_raw).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            format!("corrupt job status: {status_raw}").into(),
        )
    })?;
    let severity = row
        .get::<_, Option<String>>(7)?
        .map(|raw| {
            parse_severity(&raw).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    7,
                    rusqlite::types::Type::Text,
                    format!("corrupt job severity: {raw}").into(),
                )
            })
        })
        .transpose()?;
    let non_negative = |column: usize, name: &str, raw: i64| {
        if raw < 0 {
            Err(rusqlite::Error::FromSqlConversionFailure(
                column,
                rusqlite::types::Type::Integer,
                format!("corrupt job {name}: {raw}").into(),
            ))
        } else {
            Ok(raw as u64)
        }
    };
    let progress_bytes = row
        .get::<_, Option<i64>>(10)?
        .map(|raw| non_negative(10, "progress_bytes", raw))
        .transpose()?;
    let progress_total = row
        .get::<_, Option<i64>>(11)?
        .map(|raw| non_negative(11, "progress_total", raw))
        .transpose()?;
    let retries = row.get::<_, Option<i64>>(13)?.unwrap_or(0);
    let retries = u32::try_from(non_negative(13, "retries", retries)?).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            13,
            rusqlite::types::Type::Integer,
            format!("corrupt job retries: {retries}").into(),
        )
    })?;
    Ok(JobRecord {
        id: row.get(0)?,
        kind: row.get(1)?,
        src: row.get(2)?,
        dst: row.get(3)?,
        status,
        engine_job_id: row.get(5)?,
        error: row.get(6)?,
        severity,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        progress_bytes,
        progress_total,
        checksum: row.get(12)?,
        retries,
    })
}

/// Severity ↔ 库内字符串(核心 Display 的正逆映射;canonical 小写)。
fn parse_severity(raw: &str) -> Option<Severity> {
    match raw {
        "retryable" => Some(Severity::Retryable),
        "fatal" => Some(Severity::Fatal),
        "interrupted" => Some(Severity::Interrupted),
        _ => None,
    }
}

fn severity_raw(severity: Severity) -> &'static str {
    match severity {
        Severity::Retryable => "retryable",
        Severity::Fatal => "fatal",
        Severity::Interrupted => "interrupted",
    }
}

/// rc 载荷异常(Fatal:完整但非法 = 协议被破坏;detail 附原始载荷截断)。
fn invalid_payload(method: &str, problem: &str, reply: &Value) -> PartisyError {
    let brief: String = reply.to_string().chars().take(200).collect();
    JobErrorKind::Invalid(format!("{method}: {problem}: {brief}")).fatal()
}

/// u64 → i64 饱和夹取(字节数超 i63 不现实,防御性收口零 panic)。
fn clamp_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// 应用迁移(卡内 ②:最小框架;逐版本单事务,失败回滚后上滚;幂等;budget
/// 打开同库时复用本入口,保证迁移序列全库单源)。
pub(crate) fn run_migrations(conn: &Connection) -> Result<(), PartisyError> {
    conn.execute(SCHEMA_MIGRATIONS_DDL, ())
        .map_err(|err| db_err("create schema_migrations", err))?;
    let current: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .map_err(|err| db_err("read schema version", err))?;
    for (index, ddl) in MIGRATIONS.iter().enumerate() {
        let version = (index + 1) as i64;
        if version <= current {
            continue;
        }
        let tx = conn
            .unchecked_transaction()
            .map_err(|err| db_err("begin migration", err))?;
        let applied = tx
            // execute_batch:多语句迁移(v3 增列族)与单语句 v1/v2 统一走批执行。
            .execute_batch(ddl)
            .map_err(|err| db_err("apply migration", err))
            .and_then(|_| {
                tx.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    rusqlite::params![version, now_parts()?.0],
                )
                .map_err(|err| db_err("record migration", err))
            });
        match applied {
            Ok(_) => tx.commit().map_err(|err| db_err("commit migration", err))?,
            // 回滚失败优先上浮(DB 状态未知,零吞错)。
            Err(step_err) => {
                return Err(match tx.rollback() {
                    Ok(()) => step_err,
                    Err(rollback_err) => db_err("rollback migration", rollback_err),
                });
            }
        }
    }
    Ok(())
}

/// 平台数据目录(与 engine installer 同语义自实现,ADR-0004 否决 dirs 同款;
/// linux=XDG_DATA_HOME(仅绝对路径)→ $HOME/.local/share;macOS/win 各自已知
/// 目录;平台中立,零 unix 专属 API;budget 同语义复用)。
pub(crate) fn platform_data_dir() -> Option<PathBuf> {
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
        match env_dir("XDG_DATA_HOME") {
            Some(dir) if dir.is_absolute() => Some(dir),
            _ => env_dir("HOME").map(|home| home.join(".local").join("share")),
        }
    }
}

/// 当前时刻:(RFC3339 UTC 毫秒, unix 毫秒);时钟早于 epoch = Fatal。
fn now_parts() -> Result<(String, u64), PartisyError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| JobErrorKind::Invalid(format!("clock before epoch: {err}")).fatal())?
        .as_millis() as u64;
    Ok((rfc3339_from_millis(millis), millis))
}

/// RFC3339 UTC(毫秒精度;std 无格式化器,civil 算法自实现;锚向量单测因预算
/// 收敛未覆盖,生产行经集成测试间接行使,补强任务已登记——审查 [medium] 修正)。
fn rfc3339_from_millis(millis: u64) -> String {
    let seconds = (millis / 1000) as i64;
    let ms = (millis % 1000) as u32;
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    let second_of_day = seconds.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{ms:03}Z",
        hour = second_of_day / 3_600,
        minute = (second_of_day % 3_600) / 60,
        second = second_of_day % 60,
    )
}

/// 天数(自 1970-01-01,可负)→ (年, 月, 日)(Howard Hinnant civil_from_days)。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// jobs.id(uuid v7 意图):unix 毫秒前缀(时间有序)+ std RandomState 熵种
/// SipHash 混合纳秒/pid/计数器的随机后缀(零依赖自实现)。
fn new_job_id(now_millis: u64) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let mut hasher = RandomState::new().build_hasher();
    now_millis.hash(&mut hasher);
    nanos.hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    seq.hash(&mut hasher);
    format!("{now_millis:013}-{digest:016x}", digest = hasher.finish())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::{JobErrorKind::*, JobManager, JobStatus::*, *};
    use crate::error::Severity as Sev;

    fn mem_manager() -> JobManager {
        let conn = Connection::open_in_memory().expect("in-memory db opens");
        run_migrations(&conn).expect("migrations apply");
        JobManager {
            conn: Mutex::new(conn),
            retry: RetryPolicy::default(),
        }
    }

    /// 定档管理器(重试上限可配;其余同 [`mem_manager`])。
    fn mem_manager_with(policy: RetryPolicy) -> JobManager {
        let conn = Connection::open_in_memory().expect("in-memory db opens");
        run_migrations(&conn).expect("migrations apply");
        JobManager {
            conn: Mutex::new(conn),
            retry: policy,
        }
    }

    /// Fake rc 分发(仅测试):按脚本依序弹回复;记录 (method, params) 供断言。
    struct FakeScriptDispatch {
        replies: RefCell<VecDeque<Value>>,
        calls: RefCell<Vec<(String, Value)>>,
    }

    impl FakeScriptDispatch {
        fn scripted(replies: &[Value]) -> Self {
            FakeScriptDispatch {
                replies: RefCell::new(replies.iter().cloned().collect()),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.borrow().len()
        }
    }

    impl RcDispatch for FakeScriptDispatch {
        fn call(&self, method: &str, params: &Value) -> Result<Value, PartisyError> {
            self.calls
                .borrow_mut()
                .push((method.to_owned(), params.clone()));
            self.replies
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| JobErrorKind::Invalid("script exhausted".into()).fatal())
        }
    }

    /// 引擎提交拒绝的 Fake(severity 保真;submit 残留窗口测试用)。
    struct FakeRejectingDispatch(Sev);

    impl RcDispatch for FakeRejectingDispatch {
        fn call(&self, _method: &str, _params: &Value) -> Result<Value, PartisyError> {
            Err(PartisyError::new(self.0))
        }
    }

    /// job/status 失败应答(finished+success=false+error 文本)。
    fn failed(error: &str) -> Value {
        serde_json::json!({ "finished": true, "success": false, "error": error, "output": {} })
    }

    /// job/status 运行中应答。
    fn running() -> Value {
        serde_json::json!({ "finished": false, "success": false, "error": "" })
    }

    /// 空参提交铺设(脚本化 jobid;仅测试造数用)。
    fn submit_ok(mgr: &JobManager, jobid: i64) -> JobRecord {
        let dispatch = FakeScriptDispatch::scripted(&[serde_json::json!({ "jobid": jobid })]);
        mgr.submit(
            &dispatch,
            "sync/copy",
            "copy",
            "a:",
            "b:",
            &serde_json::json!({}),
        )
        .expect("submit")
    }

    #[test]
    fn state_machine_table_and_store_gates_hold() {
        // 卡内 ⑥:严格四态表驱动(合法集恰 3 对);库层守卫拒终态出边(结构化
        // Fatal);queued→error 合法链落 error+文本+severity。
        let all = [Queued, Running, Done, Error];
        let legal = [(Queued, Running), (Running, Done), (Running, Error)];
        for from in all {
            for to in all {
                assert_eq!(from.can_transition(to), legal.contains(&(from, to)));
            }
        }
        let mgr = mem_manager();
        let seed = "INSERT INTO jobs (id, kind, src, dst, status, engine_job_id, error, severity,
             created_at, updated_at) VALUES ('j1', 'copy', 'a:', 'b:', 'queued', 1, NULL, NULL,
             '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')";
        mgr.lock()
            .expect("lock")
            .execute(seed, [])
            .expect("seed j1");
        mgr.advance("j1", Queued, Running, None)
            .expect("queued→running");
        mgr.advance("j1", Running, Done, None)
            .expect("running→done");
        let err = mgr
            .advance("j1", Done, Running, None)
            .expect_err("no out-edge");
        assert_eq!(err.severity, Severity::Fatal);
        assert!(matches!(
            kind_of(&err).expect("structured kind"),
            IllegalTransition { from: Done, .. }
        ));
        mgr.lock()
            .expect("lock")
            .execute(
                "INSERT INTO jobs (id, kind, src, dst, status, engine_job_id, error, severity,
             created_at, updated_at) VALUES ('j2', 'copy', 'a:', 'b:', 'queued', 2, NULL, NULL,
             '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
                [],
            )
            .expect("seed j2");
        mgr.advance("j2", Queued, Error, Some(("boom", Severity::Fatal)))
            .expect("queued→error via legal chain");
        let record = mgr.get("j2").expect("row");
        assert_eq!(
            (record.status, record.error.as_deref(), record.severity),
            (Error, Some("boom"), Some(Severity::Fatal))
        );
        // j1(done)无 error/severity 的终态洁净性由集成测试对账面覆盖。
    }

    #[test]
    fn migrations_create_latest_version_and_are_idempotent() {
        // 卡内 ⑥:首开 = 最新版(框架表+jobs v1+budget_state v2+v3 增列,只追加);
        // 列集对照(v3 四列齐);重跑幂等。
        fn version(conn: &Connection) -> i64 {
            conn.query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
            .expect("version")
        }
        let mgr = mem_manager();
        {
            let conn = mgr.lock().expect("lock");
            assert_eq!(version(&conn), 3);
            let columns: Vec<String> = conn
                .prepare("SELECT name FROM pragma_table_info('jobs') ORDER BY cid")
                .expect("pragma prepare")
                .query_map([], |row| row.get(0))
                .expect("pragma query")
                .collect::<Result<Vec<String>, _>>()
                .expect("pragma collect");
            for column in ["progress_bytes", "progress_total", "checksum", "retries"] {
                assert!(
                    columns.iter().any(|name| name == column),
                    "missing {column}"
                );
            }
        }
        run_migrations(&mgr.lock().expect("relock")).expect("re-run idempotent");
        assert_eq!(
            version(&mgr.lock().expect("relock2")),
            3,
            "migration re-run is no-op"
        );
    }

    #[test]
    fn v3_migration_keeps_v2_rows_readably() {
        // 卡内 ①:旧行 NULL 兼容——手工构造 v2 形态库 + 旧数据行,run_migrations
        // 只追加 v3 后旧行可读(progress/checksum = NULL,retries = 0)。
        let conn = Connection::open_in_memory().expect("in-memory db opens");
        conn.execute_batch(SCHEMA_MIGRATIONS_DDL)
            .expect("framework table");
        conn.execute_batch(MIGRATIONS[0]).expect("v1 DDL");
        conn.execute_batch(crate::budget::BUDGET_STATE_V1)
            .expect("v2 DDL");
        conn.execute_batch(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (1, 't');
             INSERT INTO schema_migrations (version, applied_at) VALUES (2, 't');
             INSERT INTO jobs (id, kind, src, dst, status, engine_job_id, error, severity,
              created_at, updated_at) VALUES ('old1', 'copy', 'a:', 'b:', 'done', 7, NULL, NULL,
              '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
        )
        .expect("v2 shape seed");
        run_migrations(&conn).expect("v3 applies");
        let mgr = JobManager {
            conn: Mutex::new(conn),
            retry: RetryPolicy::default(),
        };
        let record = mgr.get("old1").expect("old row reads");
        assert_eq!(record.status, Done);
        assert_eq!(record.progress_bytes, None);
        assert_eq!(record.progress_total, None);
        assert_eq!(record.checksum, None);
        assert_eq!(record.retries, 0);
    }

    #[test]
    fn engine_failure_classification_matches_anchored_table() {
        // 卡内 ②:分类表驱动——全部样例 = 本机 rclone 1.75.1 实测 job/status error
        // 文本(2026-10-10,webdav 假源/rc 参数缺失/假引擎端口,详见 run 报告);
        // 未知文本保守 Fatal;Fatal 签名优先于 Retryable。
        let anchored: &[(&str, Sev)] = &[
            // rc 参数缺失
            ("Didn't find key \"srcFs\" in input", Sev::Fatal),
            // 源目录不存在
            ("directory not found", Sev::Fatal),
            // 5xx 族
            (
                "couldn't list files: 500 Internal Server Error",
                Sev::Retryable,
            ),
            ("couldn't list files: 502 Bad Gateway", Sev::Retryable),
            (
                "couldn't list files: 503 Service Unavailable",
                Sev::Retryable,
            ),
            // 429
            ("couldn't list files: 429 Too Many Requests", Sev::Retryable),
            // 权限
            ("couldn't list files: 403 Forbidden", Sev::Fatal),
            ("couldn't list files: 401 Unauthorized", Sev::Fatal),
            // 瞬态网络
            (
                "couldn't list files: Propfind \"http://127.0.0.1:19999/\": dial tcp \
                 127.0.0.1:19999: connect: connection refused",
                Sev::Retryable,
            ),
            (
                "couldn't list files: Propfind \"http://127.0.0.1:18004/\": net/http: \
                 timeout awaiting response headers",
                Sev::Retryable,
            ),
            (
                "couldn't list files: Propfind \"http://127.0.0.1:18008/\": read tcp \
                 127.0.0.1:54962->127.0.0.1:18008: read: connection reset by peer",
                Sev::Retryable,
            ),
            // http 后端目录误判(参数/路径错)
            ("is a file not a directory", Sev::Fatal),
        ];
        for (text, expected) in anchored {
            assert_eq!(classify_engine_failure(text), *expected, "{text}");
        }
        assert_eq!(
            classify_engine_failure("mystery engine failure"),
            Sev::Fatal
        );
        assert_eq!(
            classify_engine_failure("403 Forbidden after 500 Internal Server Error"),
            Sev::Fatal,
            "Fatal 签名优先"
        );
    }

    #[test]
    fn retryable_failures_requeue_until_cap_then_settle() {
        // 卡内 ②:Retryable 自动重试——失败回 queued(清锚、retries+1、失败标注
        // 保留);上限内 revive 复活再跑;超限落 error 终态(severity 保真 retryable)。
        let mgr = mem_manager_with(RetryPolicy {
            max_retries: 1,
            backoff: Duration::from_secs(60),
        });
        let params = serde_json::json!({ "srcFs": "a:", "dstFs": "b:" });
        let dispatch = FakeScriptDispatch::scripted(&[serde_json::json!({ "jobid": 11 })]);
        let job = mgr
            .submit(&dispatch, "sync/copy", "copy", "a:", "b:", &params)
            .expect("submit");
        assert_eq!(job.engine_job_id, Some(11));
        // 第一轮:失败文本 = 实测 502 形状 → Retryable 回队。
        let fail1 = FakeScriptDispatch::scripted(&[failed("couldn't list files: 502 Bad Gateway")]);
        let (record, ret) = mgr.poll(&fail1, &job.id).expect("poll 1");
        assert!(ret.is_none());
        assert_eq!(
            (
                record.status,
                record.engine_job_id,
                record.retries,
                record.severity
            ),
            (Queued, None, 1, Some(Sev::Retryable))
        );
        assert_eq!(
            record.error.as_deref(),
            Some("couldn't list files: 502 Bad Gateway")
        );
        // 回队后(无锚)poll 零引擎触达原样返回。
        let probe = FakeScriptDispatch::scripted(&[]);
        let (record, ret) = mgr.poll(&probe, &job.id).expect("poll parked");
        assert!(ret.is_none() && record.status == Queued);
        assert_eq!(probe.call_count(), 0, "无锚行零引擎触达");
        // 复活(卡内 ③)→ 新锚,失败标注清空,累计保留。
        let revive = FakeScriptDispatch::scripted(&[serde_json::json!({ "jobid": 12 })]);
        let revived = mgr
            .revive_parked(&revive, &job.id, "sync/copy", &params)
            .expect("revive");
        assert_eq!(revived.engine_job_id, Some(12));
        assert_eq!((revived.error.as_deref(), revived.severity), (None, None));
        assert_eq!(revived.retries, 1, "复活不清累计");
        // 第二轮先 running 再失败:retries(1) >= max(1) → error 终态,不再回队。
        let chain = FakeScriptDispatch::scripted(&[
            running(),
            failed("couldn't list files: 429 Too Many Requests"),
        ]);
        let (record, _) = mgr.poll(&chain, &job.id).expect("poll 2 running");
        assert_eq!(record.status, Running);
        let (record, _) = mgr.poll(&chain, &job.id).expect("poll 2 failed");
        assert_eq!(
            (record.status, record.severity, record.retries),
            (Error, Some(Sev::Retryable), 1)
        );
        assert_eq!(record.engine_job_id, Some(12), "终态保留引擎锚");
        // 终态幂等零触达。
        let probe = FakeScriptDispatch::scripted(&[]);
        let _ = mgr.poll(&probe, &job.id).expect("poll terminal");
        assert_eq!(probe.call_count(), 0);
    }

    #[test]
    fn fatal_failure_never_autoretries() {
        // 卡内 ② 禁止项:Fatal(实测 403 文本)直接 error 终态,零自动回队。
        let mgr = mem_manager();
        let job = submit_ok(&mgr, 21);
        let fail = FakeScriptDispatch::scripted(&[failed("couldn't list files: 403 Forbidden")]);
        let (record, _) = mgr.poll(&fail, &job.id).expect("poll");
        assert_eq!(
            (record.status, record.severity, record.retries),
            (Error, Some(Sev::Fatal), 0)
        );
    }

    #[test]
    fn revive_rejects_non_parked_rows_and_undispatched_methods() {
        // 卡内 ③ 门:非 queued/带锚行拒绝;method 出白名单本地拒绝;成功路径见
        // retryable_failures_requeue_until_cap_then_settle。
        let mgr = mem_manager();
        let parked = mgr.register_queued("copy", "a:", "b:").expect("parked");
        let no_reply = || FakeScriptDispatch::scripted(&[]);
        let err = mgr
            .revive_parked(
                &no_reply(),
                &parked.id,
                "job/status",
                &serde_json::json!({}),
            )
            .expect_err("method gate");
        assert_eq!(err.severity, Sev::Fatal);
        assert!(matches!(kind_of(&err), Some(MethodRejected(_))));
        // 带锚 queued(submit 窗口已回填)与终态行(零出边)拒绝复活。
        let anchored = submit_ok(&mgr, 31);
        mgr.advance(&anchored.id, Queued, Running, None)
            .expect("to running");
        mgr.advance(&anchored.id, Running, Done, None)
            .expect("to done");
        let err = mgr
            .revive_parked(
                &no_reply(),
                &anchored.id,
                "sync/copy",
                &serde_json::json!({}),
            )
            .expect_err("done row not revivable");
        assert!(matches!(kind_of(&err), Some(Invalid(_))));
        // parked 行复活成功(独立路径:未经 submit 的纯停放行)。
        let revive = FakeScriptDispatch::scripted(&[serde_json::json!({ "jobid": 32 })]);
        let revived = mgr
            .revive_parked(&revive, &parked.id, "sync/copy", &serde_json::json!({}))
            .expect("revive parked");
        assert_eq!(revived.engine_job_id, Some(32));
        assert_eq!(revived.status, Queued);
        // 带锚 queued 的拒绝分支:再造一枚 submit 窗口行(queued+锚)。
        let anchored2 = submit_ok(&mgr, 33);
        let err = mgr
            .revive_parked(
                &no_reply(),
                &anchored2.id,
                "sync/copy",
                &serde_json::json!({}),
            )
            .expect_err("anchored queued not revivable");
        assert!(matches!(kind_of(&err), Some(Invalid(_))));
    }

    #[test]
    fn cancel_rules_terminal_error_interrupted() {
        // 卡内 ④:parked 行取消零引擎触达,终态=error/user_canceled/Interrupted;
        // 带锚 running 行先 job/stop(参数=jobid)再同款终态;终态行取消=非法迁移。
        let mgr = mem_manager();
        let parked = mgr.register_queued("copy", "a:", "b:").expect("parked");
        let untouched = FakeScriptDispatch::scripted(&[]);
        let canceled = mgr
            .job_cancel(&untouched, &parked.id)
            .expect("cancel parked");
        assert_eq!(untouched.call_count(), 0, "无锚取消零引擎触达");
        assert_eq!(canceled.status, Error);
        assert_eq!(canceled.error.as_deref(), Some("user_canceled"));
        assert_eq!(canceled.severity, Some(Sev::Interrupted));
        // 带锚:job/stop 后裁决。
        let job = submit_ok(&mgr, 41);
        let stopper = FakeScriptDispatch::scripted(&[serde_json::json!({})]);
        let canceled = mgr.job_cancel(&stopper, &job.id).expect("cancel running");
        assert_eq!(
            (canceled.status, canceled.error.as_deref()),
            (Error, Some("user_canceled"))
        );
        assert_eq!(canceled.severity, Some(Sev::Interrupted));
        let calls = stopper.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "job/stop");
        assert_eq!(calls[0].1, serde_json::json!({ "jobid": 41 }));
        // 终态行取消 = 非法迁移 Fatal(零出边)。
        let err = mgr
            .job_cancel(&FakeScriptDispatch::scripted(&[]), &job.id)
            .expect_err("terminal cancel rejected");
        assert_eq!(err.severity, Sev::Fatal);
        assert!(matches!(
            kind_of(&err),
            Some(IllegalTransition { from: Error, .. })
        ));
    }

    #[test]
    fn submit_window_leaves_revivable_residue_on_engine_failure() {
        // 卡内 ⑥:submit 原子窗口——引擎提交失败(如引擎失联)残留 queued 无锚行,
        // 恢复后 revive_parked 兜底复活;本地拒绝(白名单外)零残留。
        let mgr = mem_manager();
        let err = mgr
            .submit(
                &FakeRejectingDispatch(Sev::Retryable),
                "sync/copy",
                "copy",
                "a:",
                "b:",
                &serde_json::json!({}),
            )
            .expect_err("engine submit fails");
        assert_eq!(err.severity, Sev::Retryable);
        let queued = mgr.list_jobs(Some(Queued)).expect("list queued");
        assert_eq!(queued.len(), 1, "残留 queued 无锚行");
        assert!(queued[0].engine_job_id.is_none());
        let revive = FakeScriptDispatch::scripted(&[serde_json::json!({ "jobid": 51 })]);
        let revived = mgr
            .revive_parked(&revive, &queued[0].id, "sync/copy", &serde_json::json!({}))
            .expect("revive residue");
        assert_eq!(revived.engine_job_id, Some(51));
        // 白名单外方法在落库前拒绝(零残留)。
        let err = mgr
            .submit(
                &FakeScriptDispatch::scripted(&[]),
                "operations/list",
                "copy",
                "a:",
                "b:",
                &serde_json::json!({}),
            )
            .expect_err("method gate");
        assert!(matches!(kind_of(&err), Some(MethodRejected(_))));
        assert_eq!(mgr.list_jobs(None).expect("list all").len(), 1);
    }

    #[test]
    fn list_jobs_filters_by_status_and_orders_stably() {
        // 卡内 ⑤:公开 list_jobs——None 全量/Some 过滤,序 = (created_at, id) 字典序
        // (同毫秒创建的行按 id 稳定序,断言不依赖创建时序)。
        let mgr = mem_manager();
        mgr.register_queued("copy", "a:", "b:").expect("a");
        let b = mgr.register_queued("copy", "c:", "d:").expect("b");
        mgr.advance(&b.id, Queued, Running, None)
            .expect("b running");
        fn keyed(records: &[JobRecord]) -> Vec<(&str, &str)> {
            let mut pairs: Vec<(&str, &str)> = records
                .iter()
                .map(|r| (r.created_at.as_str(), r.id.as_str()))
                .collect();
            pairs.sort_unstable();
            pairs
        }
        let all = mgr.list_jobs(None).expect("list all");
        assert_eq!(all.len(), 2);
        let unsorted: Vec<(&str, &str)> = all
            .iter()
            .map(|r| (r.created_at.as_str(), r.id.as_str()))
            .collect();
        assert_eq!(unsorted, keyed(&all), "序必须为 (created_at, id) 字典序");
        let running = mgr.list_jobs(Some(Running)).expect("list running");
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].id, b.id);
        assert!(mgr.list_jobs(Some(Done)).expect("list done").is_empty());
    }

    #[test]
    fn progress_and_checksum_write_api_roundtrips() {
        // 卡内 ① 写 API:进度/校验和落列可读;未知 id = Fatal;终态行写口仍开
        // (校验和管线可能晚于终态,保真不丢)。
        let mgr = mem_manager();
        let job = mgr.register_queued("copy", "a:", "b:").expect("parked");
        mgr.update_progress(&job.id, 100, Some(200))
            .expect("progress");
        let record = mgr.get(&job.id).expect("row");
        assert_eq!(record.progress_bytes, Some(100));
        assert_eq!(record.progress_total, Some(200));
        mgr.update_progress(&job.id, 150, None)
            .expect("progress no total");
        assert_eq!(mgr.get(&job.id).expect("row").progress_total, None);
        mgr.set_checksum(&job.id, "sha256:deadbeef")
            .expect("checksum");
        assert_eq!(
            mgr.get(&job.id).expect("row").checksum.as_deref(),
            Some("sha256:deadbeef")
        );
        let err = mgr
            .update_progress("missing-job", 1, None)
            .expect_err("unknown id");
        assert_eq!(err.severity, Sev::Fatal);
        let err = mgr
            .set_checksum("missing-job", "x")
            .expect_err("unknown id");
        assert_eq!(err.severity, Sev::Fatal);
    }

    #[test]
    fn mark_verify_failed_lands_done_rows_and_is_idempotent() {
        // T03 校验通道:done → error(severity 落列保真);非 done = Fatal 零触达;
        // 重复失败幂等返回首次裁决;checksum 列不动。
        let mgr = mem_manager();
        // ① done 行:hash 差异(Fatal)落 error 终态。
        let job = submit_ok(&mgr, 61);
        mgr.advance(&job.id, Queued, Running, None)
            .expect("running");
        mgr.advance(&job.id, Running, Done, None).expect("done");
        let landed = mgr
            .mark_verify_failed(&job.id, "checksum mismatch (sha1): src=a dst=b", Sev::Fatal)
            .expect("land");
        assert_eq!(
            (landed.status, landed.error.as_deref(), landed.severity),
            (
                Error,
                Some("checksum mismatch (sha1): src=a dst=b"),
                Some(Sev::Fatal)
            )
        );
        // ② 重复校验失败:幂等返回现行记录,首裁不被覆盖。
        let again = mgr
            .mark_verify_failed(&job.id, "size mismatch: src=1 dst=2", Sev::Retryable)
            .expect("idempotent");
        assert_eq!(
            (again.status, again.error.as_deref(), again.severity),
            (
                Error,
                Some("checksum mismatch (sha1): src=a dst=b"),
                Some(Sev::Fatal)
            )
        );
        // ③ queued 行拒绝(Fatal,校验只对 done 行有意义)。
        let parked = mgr.register_queued("copy", "a:", "b:").expect("parked");
        let err = mgr
            .mark_verify_failed(&parked.id, "x", Sev::Retryable)
            .expect_err("non-done rejected");
        assert_eq!(err.severity, Sev::Fatal);
        assert!(matches!(kind_of(&err), Some(Invalid(_))));
        // ④ 尺寸差异(Retryable)同样落列(Severity 按差异性质)。
        let job2 = submit_ok(&mgr, 62);
        mgr.advance(&job2.id, Queued, Running, None)
            .expect("running");
        mgr.advance(&job2.id, Running, Done, None).expect("done");
        let landed = mgr
            .mark_verify_failed(&job2.id, "size mismatch: src=100 dst=90", Sev::Retryable)
            .expect("land");
        assert_eq!(landed.severity, Some(Sev::Retryable));
    }
}
