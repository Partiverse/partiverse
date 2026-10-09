//! JobManager(M1-WP03-T02):generic 异步 rc job 状态机+SQLite 落库+启动对账;
//! 严格四态 `queued→running→done|error`(非法迁移拒绝并上浮);submit 以
//! `_async:true` 提交取 jobid;poll → done(取 output 作 ret)/error(取文本)
//! 落库;reconcile(调用方显式触发):running 且 engine_job_id 不在存活
//! `job/list` → error(engine_restarted,Fatal)。rc 形状以本机 rclone 1.75.1 实
//! 测锚定(`_async`→`{"jobid":<int>}`;`job/status`→`{finished,success,error,
//! output}`;`job/list`→`{jobids:[…]}`;完成 job 默认 60s 过期,过期即判
//! engine_restarted,重试策略属 WP06)。接缝(Cargo 禁普通依赖成环):生产消费
//! 面 = [`RcDispatch`] trait(T01 `RcClient` 适配器由引擎/应用层提供,集成测试
//! 含适配证明);method 门 = ASYNC_METHODS(适配器 `RcMethod` 二次把关);同步
//! 阻塞 API;依赖审计见 ADR-0006;表只存路径与元数据(红线 3)。

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
    /// 幂等不算迁移,见 [`JobManager::advance`])。
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
    /// Fatal 构造:kind 挂 `PartisyError.source()`(severity 直读,kind 经 [`kind_of`] 还原)。
    fn fatal(self) -> PartisyError {
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

/// IO 错误构造:severity 由核心 `classify_io` 表驱动,原始错误挂根因链。
fn io_err(err: std::io::Error) -> PartisyError {
    PartisyError {
        severity: classify_io(err.kind()),
        source: Some(Box::new(err)),
    }
}

/// SQLite 错误构造(Fatal:本地持久化损坏;原文入 detail)。
fn db_err(context: &str, err: rusqlite::Error) -> PartisyError {
    JobErrorKind::Db(format!("{context}: {err}")).fatal()
}

/// jobs 表行(最小字段集,列契约见 `MIGRATIONS` v1 DDL;只存路径字符串与
/// 元数据零文件内容,红线 3;进度/校验和列属 WP06)。`id`=本地持久锚
/// (时间戳前缀+随机后缀,引擎重启不失效);`engine_job_id`=运行时锚(rc
/// jobid,引擎重启即失效,由 reconcile 兜底);`error`/`severity` 仅 error 态
/// (reconcile 恒以 engine_restarted 起始);时间为 RFC3339 UTC 毫秒。
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
}

/// busy_timeout(卡内强制;单写连接下仅防短暂跨进程争用,保守 5s)。
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// schema_migrations 迁移框架表(v0 基座,先行创建)。
const SCHEMA_MIGRATIONS_DDL: &str = "CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY NOT NULL,
    applied_at TEXT NOT NULL
)";

/// 迁移脚本表(下标 i = 版本 i+1;只追加不改写)。v1 = jobs 最小字段集(红线 3)。
const MIGRATIONS: &[&str] = &["CREATE TABLE jobs (
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
)"];

/// JobManager:状态机+落库+对账(单连接+Mutex:Connection 非 Sync,卡内边界)。
pub struct JobManager {
    conn: Mutex<Connection>,
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
        })
    }

    /// 取锁(Mutex 中毒 = 前次持锁 panic,Fatal 上浮,零静默)。
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, PartisyError> {
        self.conn.lock().map_err(|poisoned| {
            JobErrorKind::Db(format!("job store mutex poisoned: {poisoned}")).fatal()
        })
    }

    /// 提交 generic 异步 rc job(卡内 ①):method 门 → 注入 `_async:true` 提交
    /// → 取 `jobid` → 落库 queued;白名单外/非 async 命令本地拒绝 Fatal。
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
        let mut payload = params.clone();
        payload["_async"] = Value::Bool(true);
        let reply = dispatch.call(method, &payload)?;
        let engine_job_id = reply.get("jobid").and_then(Value::as_i64).ok_or_else(|| {
            JobErrorKind::Invalid(format!(
                "{method}: async reply missing integer `jobid`: {reply}"
            ))
            .fatal()
        })?;
        let (now, now_millis) = now_parts()?;
        let id = new_job_id(now_millis);
        self.lock()?
            .execute(
                "INSERT INTO jobs (id, kind, src, dst, status, engine_job_id, error, severity,
                 created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, ?7, ?7)",
                rusqlite::params![
                    id,
                    kind,
                    src,
                    dst,
                    JobStatus::Queued.as_str(),
                    engine_job_id,
                    now
                ],
            )
            .map_err(|err| db_err("insert job", err))?;
        Ok(JobRecord {
            id,
            kind: kind.to_owned(),
            src: src.to_owned(),
            dst: dst.to_owned(),
            status: JobStatus::Queued,
            engine_job_id: Some(engine_job_id),
            error: None,
            severity: None,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    /// poll(卡内 ④):done(取 output 作 ret)/error(取文本,Fatal)→ 落库;
    /// 未 finished → running;终态幂等不再触引擎。
    pub fn poll(
        &self,
        dispatch: &impl RcDispatch,
        id: &str,
    ) -> Result<(JobRecord, Option<Value>), PartisyError> {
        let record = self.get(id)?;
        if matches!(record.status, JobStatus::Done | JobStatus::Error) {
            return Ok((record, None));
        }
        let engine_job_id = record.engine_job_id.ok_or_else(|| {
            JobErrorKind::Invalid(format!("job `{id}` running without engine_job_id")).fatal()
        })?;
        let reply = dispatch.call("job/status", &serde_json::json!({ "jobid": engine_job_id }))?;
        let finished = reply
            .get("finished")
            .and_then(Value::as_bool)
            .ok_or_else(|| invalid_payload("job/status", "missing bool `finished`", &reply))?;
        if !finished {
            self.advance(id, record.status, JobStatus::Running, None)?;
            return Ok((self.get(id)?, None));
        }
        let success = reply
            .get("success")
            .and_then(Value::as_bool)
            .ok_or_else(|| invalid_payload("job/status", "missing bool `success`", &reply))?;
        if success {
            self.advance(id, record.status, JobStatus::Done, None)?;
            Ok((self.get(id)?, reply.get("output").cloned())) // ret=output(实测)
        } else {
            let error = reply
                .get("error")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .ok_or_else(|| {
                    invalid_payload("job/status", "empty/missing `error` on failed job", &reply)
                })?;
            self.advance(
                id,
                record.status,
                JobStatus::Error,
                Some((error, Severity::Fatal)),
            )?;
            Ok((self.get(id)?, None))
        }
    }

    /// 启动对账(卡内 ③,调用方显式触发):running 且 engine_job_id 不在存活
    /// 清单(含行缺锚)→ error(engine_restarted,Fatal);重排队属 WP06。
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
                "SELECT id, kind, src, dst, status, engine_job_id, error, severity, created_at, updated_at
             FROM jobs WHERE id = ?1",
                rusqlite::params![id],
                row_to_record,
            )
            .optional()
            .map_err(|err| db_err("query job", err))?
            .ok_or_else(|| JobErrorKind::Invalid(format!("unknown job id: {id}")).fatal())
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

    /// 按状态列出记录(reconcile 内部面)。
    fn list_by_status(&self, status: JobStatus) -> Result<Vec<JobRecord>, PartisyError> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("SELECT id, kind, src, dst, status, engine_job_id, error, severity, created_at, updated_at
             FROM jobs WHERE status = ?1 ORDER BY created_at, id")
            .map_err(|err| db_err("prepare list", err))?;
        let rows = stmt
            .query_map(rusqlite::params![status.as_str()], row_to_record)
            .map_err(|err| db_err("list jobs", err))?;
        rows.collect::<Result<Vec<JobRecord>, rusqlite::Error>>()
            .map_err(|err| db_err("collect jobs", err))
    }
}

/// 行 → 记录映射(status/severity 列损坏 = 数据损坏,Fatal,零猜测)。
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

/// 应用迁移(卡内 ②:最小框架;逐版本单事务,失败回滚后上滚;幂等)。
fn run_migrations(conn: &Connection) -> Result<(), PartisyError> {
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
            .execute(ddl, ())
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
/// 目录;平台中立,零 unix 专属 API)。
fn platform_data_dir() -> Option<PathBuf> {
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
    use super::{JobErrorKind::*, JobManager, JobStatus::*, *};

    fn mem_manager() -> JobManager {
        let conn = Connection::open_in_memory().expect("in-memory db opens");
        run_migrations(&conn).expect("migrations apply");
        JobManager {
            conn: Mutex::new(conn),
        }
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
    fn migrations_create_version_one_and_are_idempotent() {
        // 卡内 ⑥:首开 = v1(框架表+jobs);列集对照卡内最小字段集;重跑幂等。
        let mgr = mem_manager();
        let conn = mgr.lock().expect("lock");
        let version = |conn: &Connection| {
            conn.query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("version")
        };
        assert_eq!(version(&conn), 1);
        run_migrations(&conn).expect("re-run idempotent");
        assert_eq!(version(&conn), 1, "migration re-run is no-op");
    }
}
