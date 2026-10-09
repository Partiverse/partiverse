# 分支保护配置说明(main)

> 状态:配置契约。GitHub 侧配置由 Owner 执行(D35 远端未建);本文件由 M1-WP01-T03 落档,规定保护口径。

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
4. 建议:✅ Require conversation resolution before merging。
5. 建议勾选 **Include administrators**,Owner 直推同样受门禁约束(与铁律 5 一致);其余项 Owner 裁量。

> 注:GitHub UI 的状态检查下拉框需该 check 至少出现过一次才可选;首个 PR 触发 CI run 后再保存本规则,
> 或直接用下方 REST 载荷按名写入。

## REST 等效配置(gh CLI,D35 建远端后执行)

```bash
gh api repos/Partiverse/partiverse/branches/main/protection -X PUT --input - <<'EOF'
{
  "required_status_checks": { "strict": true, "contexts": ["gates (linux)"] },
  "enforce_admins": true,
  "required_pull_request_reviews": {
    "required_approving_review_count": 1,
    "dismiss_stale_reviews": true
  },
  "restrictions": null,
  "allow_force_pushes": false,
  "allow_deletions": false
}
EOF
```

> 仓库 slug 按组织惯例假定为 `Partiverse/partiverse`(姊妹仓库 `Partiverse/partisync`);
> D35 定名后如不同请替换。
