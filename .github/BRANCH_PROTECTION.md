# 分支保护配置说明(main)

> 状态:配置契约。GitHub 侧配置由 Owner 执行完毕(D35);本文件由 M1-WP01-T03 落档,
> M1-WP01-T06 已按 Owner 2026-10-09 实际生效配置逐一核对同步(证据:`gh api repos/Partiverse/partiverse/branches/main/protection`)。

## 保护目标

`main` 禁止直推;所有变更一律经 PR,且必需门禁全绿方可合入(AGENTS.md 铁律 5:门禁前置,CI 红灯禁止合入)。

## 必需状态检查(与 .github/workflows/ci.yml job 名一致)

| check 名 | 是否必需 | 说明 |
|---|---|---|
| `gates (linux)` | ✅ 必需 | 五门禁:fmt / clippy(-D warnings) / cargo test / pnpm build / cargo deny |
| `smoke (windows)` | ❌ 不必需 | Win 编译冒烟,allowed-failure(`continue-on-error`);升级为必需前按 run 报告跟进 issue 计划执行 |
| `smoke (macos)` | ❌ 不必需 | mac 编译冒烟,同上 |

## Owner 侧操作(GitHub → Settings → Branches → branch protection rules,pattern `main`)

1. ✅ **Require a pull request before merging**(建议 Required approvals = 1)。
2. ✅ **Require status checks to pass before merging** → 搜索并选中 `gates (linux)`;
   同时勾选 **Require branches to be up to date before merging**(防合并后门禁漂移)。
3. ✅ **Do not allow force pushes**;✅ **Do not allow deletions**。
4. Require conversation resolution before merging:❌ 未勾选(实测 `required_conversation_resolution=false`,
   与远端实际一致;是否启用由 Owner 另行裁量,不作为本契约必需项)。
5. **Include administrators 不勾选(即 `enforce_admins=false`)**——偏离理由见下方注记;其余项 Owner 裁量。

> 注:GitHub UI 的状态检查下拉框需该 check 至少出现过一次才可选;首个 PR 触发 CI run 后再保存本规则,
> 或直接用下方 REST 载荷按名写入。

## REST 等效配置(gh CLI;2026-10-09 已按此口径生效,重放本载荷 = 现网配置)

```bash
gh api repos/Partiverse/partiverse/branches/main/protection -X PUT --input - <<'EOF'
{
  "required_status_checks": { "strict": true, "contexts": ["gates (linux)"] },
  "enforce_admins": false,
  "required_pull_request_reviews": {
    "required_approving_review_count": 1,
    "dismiss_stale_reviews": false
  },
  "restrictions": null,
  "allow_force_pushes": false,
  "allow_deletions": false
}
EOF
```

> **enforce_admins=false 偏离注记(Owner 裁量 2026-10-09)**:本仓库当前为单账号(Partiverse 唯一提交者),
> 若 `enforce_admins=true`,Owner 自开 PR 无人可批准(required approvals=1),保护规则将封锁一切合入(自批死锁)。
> 该项属文档建议项、非 CI 门禁(必需检查不受影响),故按实际配置关闭;待出现第二维护者后重评开启。
>
> 2026-10-09 逐一核对结果(M1-WP01-T06,`gh api` 实测远端):strict=true ✓、contexts=`["gates (linux)"]` ✓、
> required approvals=1 ✓、enforce_admins=false ✓(偏离如上)、dismiss_stale_reviews=false(文档原载 true,已同步实际)、
> conversation resolution=false(见上第 4 条)、force pushes 禁止 ✓、deletions 禁止 ✓。
>
> 仓库 slug 已核实为 `Partiverse/partiverse`(origin 实测;姊妹仓库 `Partiverse/partisync`)。
