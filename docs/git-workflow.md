# Git 工作流

## 分支与 PR

- 所有功能/修复 MUST 走分支流程:`fix/<描述>` / `feat/<描述>` / `docs/<描述>` 分支 → 本地验证 → 推送 → PR → 合并后删除远程分支
- 禁止直接向 main 提交推送。事故先例:直接在 main 上推送三项修复,导致无分支可删、dependabot PR 联动丢失(rcgen 迁移本应让 dependabot PR 自动关闭,却只能事后依赖 rebase)
- 唯一例外:需在 main 手动触发 CI 验证的场景(GitHub Actions `workflow_dispatch` 只支持默认分支,如 fuzz workflow)。此类改动 MUST 先说明理由并获得确认,再直接推 main

## 提交

- 提交格式:`<类型>(<范围>): <简要描述>`(中文),如 `fix(ci): ...`、`chore(deps): ...`、`docs(agents): ...`
- 提交前完整流程:`cargo fmt --all` -> `cargo build --all`(零警告) -> `cargo nextest run --all`(全通过)
- 如因环境/耗时无法完整验证,MUST 在回复中说明未运行项和风险,禁止声称已验证
- 修复 CI/CD 报错后再推送

## 推送

- 用户要求推送或提交需要远端交付时再执行 `git push`
- `git push` 若 SSH 失败(Connection reset/timeout),MUST 立即回退 HTTPS:
  `git remote set-url origin https://github.com/baiye2941/ai-model-downloader.git && git push && git remote set-url origin git@github.com:baiye2941/ai-model-downloader.git`
  推送完毕后 MUST 切回 SSH remote

## 合并与清理

- PR 合并后 MUST 删除远程分支:`git push origin --delete <branch>`
- 提交/合并前确认无残留分支:`git ls-remote --heads origin`(除 main 与 open PR 分支)
