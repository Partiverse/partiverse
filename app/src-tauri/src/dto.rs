//! IPC 数据传输对象(M1-WP05-T01):crate 领域类型 → specta 强类型线格式。
//! 壳薄纪律:仅字段映射零业务逻辑;字段语义以 crates 源类型注释为准,此处不重复。

use partiverse_core::budget::{Decision, GatedSubmit};
use partiverse_core::jobs::{JobRecord, JobStatus};
use partiverse_core::schema::{FieldDescription, ProviderForm};
use partiverse_engine::coordinator::EngineHandle;
use partiverse_engine::rcd::RcdState;
use serde::{Deserialize, Serialize};
use specta::Type;
use specta_typescript::Number;

use crate::error::Severity;

/// 引擎进程状态(rcd.rs `RcdState` 线格式镜像)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum EngineState {
    Starting,
    Ready,
    Stopping,
    Exited,
    Failed,
}

impl From<RcdState> for EngineState {
    fn from(value: RcdState) -> Self {
        match value {
            RcdState::Starting => Self::Starting,
            RcdState::Ready => Self::Ready,
            RcdState::Stopping => Self::Stopping,
            RcdState::Exited => Self::Exited,
            RcdState::Failed => Self::Failed,
        }
    }
}

/// 引擎活动槽位快照(`EngineHandle` 观测面;`rc_client` 凭据载体不入线格式;
/// 快照口径 = ensure 时刻观测值,零引擎触达)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct EngineSnapshot {
    pub slot_id: String,
    pub state: EngineState,
    pub pid: Option<u32>,
    pub socket_path: String,
}

impl From<&EngineHandle> for EngineSnapshot {
    fn from(handle: &EngineHandle) -> Self {
        Self {
            slot_id: handle.slot_id.clone(),
            state: EngineState::from(handle.state),
            pid: handle.pid,
            socket_path: handle.socket_path.display().to_string(),
        }
    }
}

/// 连接表单单字段描述(core `FieldDescription` 七属性直映)。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct FieldDesc {
    pub name: String,
    pub field_type: String,
    pub required: bool,
    pub is_password: bool,
    pub advanced: bool,
    pub exclusive: bool,
    /// 默认值(JSON 文本,null → 无):specta 的 `Value` Type 实现为递归内联
    /// 类型,导出期无限递归(128MB 栈实测溢出),动态 JSON 一律文本过线。
    pub default: Option<String>,
}

impl From<&FieldDescription> for FieldDesc {
    fn from(field: &FieldDescription) -> Self {
        Self {
            name: field.name.clone(),
            field_type: field.field_type.clone(),
            required: field.required,
            is_password: field.is_password,
            advanced: field.advanced,
            exclusive: field.exclusive,
            default: match &field.default {
                serde_json::Value::Null => None,
                value => Some(value.to_string()),
            },
        }
    }
}

/// 单个 provider 的连接表单 schema(core `ProviderForm` 线格式)。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct ProviderFormOut {
    pub name: String,
    pub description: String,
    pub fields: Vec<FieldDesc>,
}

impl From<&ProviderForm> for ProviderFormOut {
    fn from(form: &ProviderForm) -> Self {
        Self {
            name: form.name.clone(),
            description: form.description.clone(),
            fields: form.fields.iter().map(FieldDesc::from).collect(),
        }
    }
}

/// job 状态(core `JobStatus` 线格式镜像;Deserialize 供命令参数面反序列化)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum JobStatusOut {
    Queued,
    Running,
    Done,
    Error,
}

impl From<JobStatus> for JobStatusOut {
    fn from(value: JobStatus) -> Self {
        match value {
            JobStatus::Queued => Self::Queued,
            JobStatus::Running => Self::Running,
            JobStatus::Done => Self::Done,
            JobStatus::Error => Self::Error,
        }
    }
}

impl JobStatusOut {
    /// 线格式 → core 状态(job_list 过滤参数回映射;形状一一对应零丢失)。
    #[must_use]
    pub fn to_core(self) -> JobStatus {
        match self {
            Self::Queued => JobStatus::Queued,
            Self::Running => JobStatus::Running,
            Self::Done => JobStatus::Done,
            Self::Error => JobStatus::Error,
        }
    }
}

/// jobs 表行(core `JobRecord` 直映;零文件内容,红线 3)。
/// M1-WP06-T02 增补传输面四列:进度(采样来源 core/stats 组)/校验和(待
/// 哈希管线,白名单无哈希查询命令,登记非静默)/自动重试累计。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct JobRecordOut {
    pub id: String,
    pub kind: String,
    pub src: String,
    pub dst: String,
    pub status: JobStatusOut,
    /// rc jobid:specta-typescript 禁导 i64,经官方 `Option<Number>` 标注
    /// (jobid 远小于 2^53,精度损失不可达;可空语义保留——停放行 None)。
    #[specta(type = Option<Number>)]
    pub engine_job_id: Option<i64>,
    pub error: Option<String>,
    pub severity: Option<Severity>,
    pub created_at: String,
    pub updated_at: String,
    /// 已传字节(core/stats 组采样;None = 未采样)。
    // u64 禁导(specta BigInt 禁令):`Option<Number>` 标注保住可空语义
    // (engine_job_id 先例的 `Number` 会吞掉 null,进度列 None=未采样不可吞)。
    #[specta(type = Option<Number>)]
    pub progress_bytes: Option<u64>,
    /// 预估总字节(None = 不可比)。
    #[specta(type = Option<Number>)]
    pub progress_total: Option<u64>,
    /// 校验和标注(None = 未计算;哈希管线另卡)。
    pub checksum: Option<String>,
    /// Retryable 自动回队累计。
    pub retries: u32,
}

impl From<JobRecord> for JobRecordOut {
    fn from(record: JobRecord) -> Self {
        Self {
            id: record.id,
            kind: record.kind,
            src: record.src,
            dst: record.dst,
            status: JobStatusOut::from(record.status),
            engine_job_id: record.engine_job_id,
            error: record.error,
            severity: record.severity.map(Severity::from),
            created_at: record.created_at,
            updated_at: record.updated_at,
            progress_bytes: record.progress_bytes,
            progress_total: record.progress_total,
            checksum: record.checksum,
            retries: record.retries,
        }
    }
}

/// poll 结果:记录 + done 态 output 载荷(JSON 文本,其余 null)。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct JobPollOut {
    pub record: JobRecordOut,
    pub output: Option<String>,
}

impl From<(JobRecord, Option<serde_json::Value>)> for JobPollOut {
    fn from((record, output): (JobRecord, Option<serde_json::Value>)) -> Self {
        Self {
            record: JobRecordOut::from(record),
            output: output.map(|value| value.to_string()),
        }
    }
}

/// 预算 acquire 决策(core `Decision`;`Number` 标注理由同 JobRecordOut)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum BudgetDecision {
    Allow,
    Throttled {
        /// 可重试时刻(epoch 毫秒)。
        #[specta(type = Number)]
        wait_until_ms: u64,
    },
    Exhausted,
}

impl From<Decision> for BudgetDecision {
    fn from(value: Decision) -> Self {
        match value {
            Decision::Allow => Self::Allow,
            Decision::Throttled { wait_until_ms } => Self::Throttled { wait_until_ms },
            Decision::Exhausted => Self::Exhausted,
        }
    }
}

/// 预算门提交结果(core `GatedSubmit`;Exhausted=job 留 queued 零引擎触达)。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum GatedSubmitOut {
    Submitted(JobRecordOut),
    Throttled {
        /// 可重试时刻(epoch 毫秒)。
        #[specta(type = Number)]
        wait_until_ms: u64,
    },
    Exhausted(JobRecordOut),
}

impl From<GatedSubmit> for GatedSubmitOut {
    fn from(value: GatedSubmit) -> Self {
        match value {
            GatedSubmit::Submitted(record) => Self::Submitted(JobRecordOut::from(record)),
            GatedSubmit::Throttled { wait_until_ms } => Self::Throttled { wait_until_ms },
            GatedSubmit::Exhausted(record) => Self::Exhausted(JobRecordOut::from(record)),
        }
    }
}

/// 同步文件操作结果(core `fsops::FsOpOutcome` 线格式镜像;M1-WP06-T02)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum FsOpOutcomeOut {
    Applied,
    /// 预算限流零引擎触达,可等待重试。
    Throttled {
        /// 可重试时刻(epoch 毫秒)。
        #[specta(type = Number)]
        wait_until_ms: u64,
    },
    /// 预算结构性拒绝(cost 超桶容)。
    Exhausted,
}

impl From<partiverse_core::fsops::FsOpOutcome> for FsOpOutcomeOut {
    fn from(value: partiverse_core::fsops::FsOpOutcome) -> Self {
        match value {
            partiverse_core::fsops::FsOpOutcome::Applied => Self::Applied,
            partiverse_core::fsops::FsOpOutcome::Throttled { wait_until_ms } => {
                Self::Throttled { wait_until_ms }
            }
            partiverse_core::fsops::FsOpOutcome::Exhausted => Self::Exhausted,
        }
    }
}

/// job 重试结果(M1-WP06-T02):Revived=停放行原地复活(同 id);Submitted=
/// error 终态行按预算门提交**新** job(终态零出边,旧行留痕);Throttled/
/// Exhausted = 预算门未放行(行面不变)。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum JobRetryOut {
    Revived(JobRecordOut),
    Submitted(JobRecordOut),
    Throttled {
        /// 可重试时刻(epoch 毫秒)。
        #[specta(type = Number)]
        wait_until_ms: u64,
    },
    Exhausted(JobRecordOut),
}
