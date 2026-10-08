# Workflow 任务运行模板(wp-task)

> 配套执行方案 §8(ZCode Workflow 长任务规划)。每次实例化:复制本骨架为 TypeScript workflow(CreateWorkflow),填入任务卡四件套。
> 纪律:run 永远停在人类闸门;失败产物保留;卡外决策中止上报。

## args 契约

```ts
interface WpTaskArgs {
  taskCard: string;      // docs/tasks/M1-WP01-T01.md 路径(四件套在此)
  specRef: string;       // docs/specs/M1-WP00.md#锚点
  allowedPaths: string[];// 允许触碰的文件/目录(与卡内一致,机器可校验)
  branch: string;        // m1/wp01-t01-slug
}
```

## 五段骨架

```ts
// 1 preflight:读卡→校验 allowedPaths/分支→依赖检查→预飞命令(工具链版本)
// 2 implement:实现 subagent(输入=任务卡+specRef+指定源码路径;输出=diff+说明)
//   - 卡外决策 → throw {needsOwner:true, question} 中止上报(不猜)
// 3 verify:fmt→clippy→test→pnpm build→性能预算脚本→(GUI)截图探针
//   - 失败→修复循环 ≤2 轮(仅允许改 allowedPaths 内文件)→仍败:stop,留报告
// 4 adversarial-review:独立 subagent,按禁止行为清单+DoD 逐条找错,输出 PASS/FAIL+证据
// 5 assemble:组装提交(AGENTS 提交格式)→生成 PR 描述(测试证据/截图/SPEC 勾选草案)
//   → 停:输出人类闸门清单(Owner 终审项/GUI 实操项/可选更新项)
```

## 输出产物(每 run)

- 分支上的原子提交(≤400 行,AGENTS 格式含 Task-ID/AI-Assist);
- `docs/reports/runs/{TaskID}.md`:各段结果/验证输出/审查结论/待 Owner 项。

## 并行与串行

- 同 WP 内 allowedPaths 无交集的任务可并行多 run;共享文件串行;跨 WP 不并行。
