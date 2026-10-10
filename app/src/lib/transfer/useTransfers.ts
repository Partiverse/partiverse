// 传输队列 store(M1-WP06-T02 DoD④):jobList 轮询 + 每活跃行 jobPoll(壳内
// 已升级 poll_with_progress:推进状态机 + core/stats 组采样);提交/重试/取消;
// Throttled 挂起行(零落库,客户端计时到点自动重提交,对应 §4.4「限速中-该盘
// 限流」可解释态)。错误一律上浮 store.error(禁吞错/静默降级)。
//
// 对抗审查修复轮([high] split-brain):原实现为组件内 useState/useRef,BrowseView
// 与 TransferBar 各持一份私有实例——提交链(BrowseView→TransferDialog→submit)
// 登记的重试元数据对传输条不可见(失败行内重试恒禁用+「another session」tooltip
// 事实错误)、Throttled 挂起行传输条永不显示、BrowseView 随路由卸载即销毁挂起行/
// 元数据、双实例各起 2s 轮询环(jobPoll+core/stats 双倍触达)。现改为模块级单例
// + useSyncExternalStore(同 lib/healthStore.ts 既有惯例):全部消费者共享同一份
// records/pending/重试元数据;轮询环按订阅者计数起停,恒为单环。
import { useSyncExternalStore } from "react";
import { commands, type JobRecordOut } from "@/bindings";
import {
  aggregateProgress, type Aggregate, type PendingThrottle, type QueueRow, type SubmitMeta,
} from "./model";

const POLL_MS = 2000;

function envelopeError(cmdError: { kind: string; msg: string }): string {
  return `${cmdError.kind}: ${cmdError.msg}`;
}

export function errorText(err: unknown): string {
  return err instanceof Error ? err.message : JSON.stringify(err);
}

export type SubmitVerdict = "submitted" | "throttled" | "exhausted";

export interface TransfersController {
  rows: QueueRow[];
  aggregate: Aggregate;
  /** 最近一次刷新/提交失败原因(UI 显式渲染,非静默)。 */
  error: string | null;
  /** 本会话提交元数据(job_submit/job_retry 供参;跨会话行缺失 → 重试禁用)。 */
  hasMeta: (id: string) => boolean;
  refresh: () => Promise<void>;
  submit: (meta: SubmitMeta) => Promise<SubmitVerdict>;
  retry: (id: string) => Promise<void>;
  cancel: (id: string) => Promise<void>;
  /** 审查 [medium] 修正:命令面失败显式写入错误面(UI 层 catch 后调用,禁吞错)。 */
  reportError: (message: string) => void;
}

/** 订阅快照(getSnapshot 稳定引用;状态变更时整体重建后通知)。 */
type Snapshot = Pick<TransfersController, "rows" | "aggregate" | "error">;

const EMPTY_AGGREGATE: Aggregate = { activeCount: 0, bytes: 0, totalBytes: null };

class TransfersStore {
  private records: JobRecordOut[] = [];
  private pendingList: PendingThrottle[] = [];
  private errorMessage: string | null = null;
  /** 本会话重试元数据(id→提交参):单例持有 → 跨组件/跨路由存活。 */
  private readonly meta = new Map<string, SubmitMeta>();
  private readonly listeners = new Set<() => void>();
  private snapshot: Snapshot = { rows: [], aggregate: EMPTY_AGGREGATE, error: null };
  // 定时器句柄类型随宿主环境(DOM=number / Node=Timeout),以返回类型锚定。
  private kickoff: ReturnType<typeof setTimeout> | null = null;
  private timer: ReturnType<typeof setInterval> | null = null;
  private started = false;

  // ── 订阅面(useSyncExternalStore):首个订阅者起轮询环,末个退订停环。──
  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    this.start();
    return () => {
      this.listeners.delete(listener);
      if (this.listeners.size === 0) this.stop();
    };
  };

  getSnapshot = (): Snapshot => this.snapshot;

  // 无 SSR,但 useSyncExternalStore 三参形态与 healthStore 惯例一致。
  getServerSnapshot = (): Snapshot => this.snapshot;

  hasMeta = (id: string): boolean => this.meta.has(id);

  /** 提交:成功/预算耗尽均登记重试元数据;Throttled 入挂起行(客户端计时)。 */
  submit = async (meta: SubmitMeta): Promise<SubmitVerdict> => {
    const result = await commands.jobSubmit(meta.node, meta.method, meta.kind, meta.src, meta.dst, meta.params, meta.cost);
    if (result.status === "error") throw new Error(envelopeError(result.error));
    // specta 合成并集的成员携带全部键(optional never),`in` 窄化失效,以
    // undefined 比较判别(成员字段必填,语义等价)。
    if (result.data.submitted !== undefined) {
      this.meta.set(result.data.submitted.id, meta);
      return "submitted";
    }
    if (result.data.exhausted !== undefined) {
      this.meta.set(result.data.exhausted.id, meta);
      return "exhausted";
    }
    const waitUntilMs = result.data.throttled.wait_until_ms;
    this.pendingList = [...this.pendingList, { waitUntilMs, meta }];
    this.commit();
    return "throttled";
  };

  /** 刷新一轮:清单 → 每活跃行 poll(状态机+进度)→ 挂起行到点重提交。 */
  refresh = async (): Promise<void> => {
    try {
      const listed = await commands.jobList(null);
      if (listed.status === "error") throw new Error(envelopeError(listed.error));
      const byId = new Map(listed.data.map((record) => [record.id, record]));
      for (const record of listed.data) {
        // 终态行零触达语义在壳内 poll(幂等);这里只 poll 活跃行。
        if (record.status !== "queued" && record.status !== "running") continue;
        const polled = await commands.jobPoll(record.id);
        if (polled.status === "error") throw new Error(envelopeError(polled.error));
        byId.set(polled.data.record.id, polled.data.record);
      }
      this.records = [...byId.values()].sort(
        (a, b) => a.created_at.localeCompare(b.created_at) || a.id.localeCompare(b.id),
      );
      this.commit();
      // 挂起行(限流)到点自动重提交;再限流则顺延(§4.4 已自动排队语义)。
      const now = Date.now();
      const due = this.pendingList.filter((item) => item.waitUntilMs <= now);
      if (due.length > 0) {
        // 先步进事实源再重提交:重叠刷新轮不会重复消化同一挂起行(双发竞态)。
        this.pendingList = this.pendingList.filter((item) => !due.includes(item));
        this.commit();
        for (const item of due) await this.submit(item.meta);
      }
      this.errorMessage = null;
      this.commit();
    } catch (err) {
      this.errorMessage = errorText(err);
      this.commit();
    }
  };

  /** 重试:以提交时原元数据调 job_retry(跨会话行缺元数据 → 显式抛错,禁静默)。 */
  retry = async (id: string): Promise<void> => {
    const meta = this.meta.get(id);
    if (meta === undefined) {
      throw new Error(`retry metadata unavailable for job ${id} (submitted in another session)`);
    }
    const result = await commands.jobRetry(id, meta.node, meta.method, meta.kind, meta.src, meta.dst, meta.params, meta.cost);
    if (result.status === "error") throw new Error(envelopeError(result.error));
    if (result.data.submitted !== undefined) this.meta.set(result.data.submitted.id, meta);
    if (result.data.exhausted !== undefined) this.meta.set(result.data.exhausted.id, meta);
    if (result.data.throttled !== undefined) {
      this.pendingList = [...this.pendingList, { waitUntilMs: result.data.throttled.wait_until_ms, meta }];
      this.commit();
    }
    await this.refresh();
  };

  cancel = async (id: string): Promise<void> => {
    const result = await commands.jobCancel(id);
    if (result.status === "error") throw new Error(envelopeError(result.error));
    await this.refresh();
  };

  // ── 轮询环生命周期:started 哨兵防双环;订阅计数到零才停(多消费者单环)。──
  private start(): void {
    if (this.started) return;
    this.started = true;
    // 挂载首轮经宏任务发起(禁 effect 体内同步级联 setState);后续按 POLL_MS
    // 轮询。用全局定时器(非 window.*):浏览器语义等价,vitest 假定时器可注入。
    this.kickoff = setTimeout(() => { void this.refresh(); }, 0);
    this.timer = setInterval(() => { void this.refresh(); }, POLL_MS);
  }

  private stop(): void {
    if (!this.started) return;
    this.started = false;
    if (this.kickoff !== null) {
      clearTimeout(this.kickoff);
      this.kickoff = null;
    }
    if (this.timer !== null) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }

  /** 审查 [medium] 修正:命令面失败(retry/cancel 的 rejection)显式写入错误面,
   * 与「错误一律上浮 store.error(禁吞错)」自述一致(useTransfers.ts:4)。 */
  reportError(message: string): void {
    this.errorMessage = message;
    this.commit();
  }

  /** 状态唯一提交入口:重建快照并通知订阅者(records/pending/error 全走此门)。 */
  private commit(): void {    const rows: QueueRow[] = [
      ...this.pendingList.map((item, index) => ({
        key: `pending:${item.waitUntilMs}:${index}`, record: null, pending: item,
      })),
      ...this.records.map((record) => ({
        key: `job:${record.id}`, record, pending: null as PendingThrottle | null,
      })),
    ];
    this.snapshot = { rows, aggregate: aggregateProgress(rows), error: this.errorMessage };
    for (const listener of this.listeners) listener();
  }

  /** 测试专用(仅 vitest 调用;生产代码禁止):停环+清空单例状态,保证模块级
   * 单例在测试间确定性复位。 */
  resetForTests(): void {
    this.stop();
    this.records = [];
    this.pendingList = [];
    this.errorMessage = null;
    this.meta.clear();
    this.snapshot = { rows: [], aggregate: EMPTY_AGGREGATE, error: null };
  }
}

/** 模块级单例:全应用唯一传输队列事实源(全部消费者共享,跨路由卸载存活)。 */
const sharedStore = new TransfersStore();

/** React 订阅面:全部消费者共享同一单例与同一轮询环(split-brain 修复点)。 */
export function useTransfers(): TransfersController {
  const snapshot = useSyncExternalStore(
    sharedStore.subscribe,
    sharedStore.getSnapshot,
    sharedStore.getServerSnapshot,
  );
  return {
    rows: snapshot.rows,
    aggregate: snapshot.aggregate,
    error: snapshot.error,
    hasMeta: sharedStore.hasMeta,
    refresh: sharedStore.refresh,
    submit: sharedStore.submit,
    retry: sharedStore.retry,
    cancel: sharedStore.cancel,
    reportError: sharedStore.reportError.bind(sharedStore),
  };
}

/** 测试专用重置入口(仅测试文件调用)。 */
export function resetTransfersForTests(): void {
  sharedStore.resetForTests();
}
