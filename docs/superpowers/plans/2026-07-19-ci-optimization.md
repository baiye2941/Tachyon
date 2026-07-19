# CI/CD 交叉验证最优优化方案

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 消除 CI/Release 双轨漂移与发布链路假绿，在不降低门禁强度的前提下降低重复成本，并让本地预检与 CI 同源。

**Architecture:** 策略与检查逻辑下沉到 `scripts/ci/*.sh`（SSOT），环境装配用 composite action，完整门禁图用 reusable workflow `ci-core.yml`；`ci.yml` / `release.yml` 只做触发与发布副作用；文档只引用脚本入口。

**Tech Stack:** GitHub Actions, shell scripts, cargo-llvm-cov regions, cargo-deny, bun frozen-lockfile, cosign keyless, Tauri updater ed25519

## Global Constraints

- 覆盖率门禁必须保持：**逐 crate + `--fail-under-regions 90`**，禁止改回合计 lines
- Windows 测试为生产路径（IOCP/WinFile），禁止从 PR 全量移除
- 禁止 `continue-on-error` 挂在 test / coverage / miri / publish 关键路径
- 禁止私钥缺失时软跳过 updater 签名
- 禁止把 “标 prerelease” 当作安全回滚
- 注释/文档/提交信息使用中文

---

## 交叉验证结论（四视角）

审查维度：

| Agent | 视角 |
|-------|------|
| 代码审查员 | 门禁正确性 / false green / false red |
| 安全工程师 | 发布供应链 / 签名 / 权限 |
| 性能工程师 | 成本 / wall-clock / ROI |
| 软件架构师 | SSOT / 复用 / 演进路径 |

### 共识确认（≥3 视角一致）

| # | 问题 | 正确性 | 安全 | 成本 | 架构 | 结论 |
|---|------|--------|------|------|------|------|
| 1 | dry-run 未接线 + dispatch version-check 必炸 | ✓ | ✓ | — | ✓ | **P0 真缺陷** |
| 2 | publish 顶层 glob 可导致 SHA256/cosign 空跑仍 public | ✓ | ✓ | — | — | **P0 真缺陷** |
| 3 | Release Miri 缺 `dir_sync`，与 CI 漂移 | ✓ | ✓ | — | ✓ | **P0 漂移** |
| 4 | Release 手抄 CI 子集（弱于 PR CI） | ✓ | ✓ | ✓ | ✓ | **P0/P1 根因** |
| 5 | CLAUDE.md `fail-under-lines` 与 CI regions 不一致 | ✓ | — | — | ✓ | **P1 文档假同源** |
| 6 | `bun install` 无 `--frozen-lockfile` | ✓ | ✓ | — | — | **P1 可复现性** |
| 7 | fuzz cache key=`run_id`（注释写日期） | ✓ | — | ✓ | — | **P1 成本+误导** |
| 8 | audit ignore 与 deny.toml 双份维护 | ✓ | ✓ | — | ✓ | **P1 漂移温床** |
| 9 | coverage 6 crate 循环 + HTML 再跑 | 成本为主 | — | ✓ | ✓ | **P1 成本（不降强度）** |
| 10 | Tauri pubkey=PLACEHOLDER，签名可软跳过 | — | ✓ | — | — | **P0 安全（发布能力）** |

### 单视角高价值补充

| 来源 | 发现 | 采纳 |
|------|------|------|
| 安全 | 签名应对 **Release 资产** 做字节对账，不只 workflow artifact | 采纳进 P0/P1 |
| 安全 | `publish-release` 缺 Environment + required reviewers | 采纳进第 1 周 |
| 安全 | rollback 仅 `--prerelease`，不 draft/delete，且不覆盖 publish 失败 | 采纳进第 1 周 |
| 安全 | `verify-signature-config.sh` 把 PLACEHOLDER 判 PASS，且 endpoints 检查假红 | 采纳进 P1 |
| 成本 | 热缓存 main ≈7–8 min wall / ~35 min billable；冷 20+ min | 作为基线 |
| 成本 | rust-cache 无 `shared-key`，同 OS stable job 不共享 target | 采纳进第 1 周 |
| 成本 | path filters 当前价值=0 | 采纳进第 1 周 |
| 成本 | bench 不应进 `ci-pass` 关键路径（噪声大） | 采纳（main 采样保留） |
| 架构 | L0 policy / L1 scripts / L2 composite / L3 reusable / L4 thin trigger | 作为目标分层 |
| 正确性 | `ci-pass` 把 `skipped` 当通过：bench 可接受，未来 required job 条件误 skip 会假绿 | 采纳进文档约束 |

### 明确禁止（四视角一致）

1. 用合计 `--fail-under-lines 90` 替代逐 crate regions  
2. PR 去掉 Windows 测试  
3. 扩大 nextest retries / `continue-on-error` 掩盖 flaky  
4. 私钥缺失时软跳过 `.sig`  
5. 只标 prerelease 当回滚  
6. release 只看“最近一次 main 绿”而不校验 **同 SHA**  
7. 为对齐两边而扩大 miri skip 且不写原因  

---

## 目标架构（共识）

```text
policy / deny.toml          # L0 策略数据（后期 policy.toml）
        │
        ▼
scripts/ci/*.sh             # L1 SSOT：coverage / miri / audit / frontend / version-check / preflight
        │
        ▼
.github/actions/setup-*     # L2 环境：apt / rust+cache / bun
        │
        ▼
ci-core.yml (workflow_call) # L3 完整门禁图 + ci-pass
       ▲          │
    ci.yml     release.yml  # L4 触发器
   push/PR    tag/dispatch
                  │
           version-check → build+sign → smoke+verify → (env approval) → publish
                  │
           dry_run 控制发布副作用
```

### dry-run 目标语义

```text
workflow_dispatch + dry-run=true  → 可跑门禁+构建，禁止 create/undraft public release
workflow_dispatch + dry-run=false → 需显式允许；仍建议 Environment 审批
tag push                         → 正常发布路径
version-check:
  tag     → 比 tag vs Cargo/tauri/frontend
  dispatch→ 只比三文件互相同，不解析 GITHUB_REF 当版本
```

### 发布安全目标语义

```text
build → draft release + workflow artifacts
  → smoke（递归找包 + 结构）
  → download Release 资产（或确认 artifact 字节 == release 资产）
  → SHA256 + cosign +（强制）Tauri .sig
  → 硬断言：产物数 == 校验数 == bundle 数（>=1）
  → Environment 审批
  → undraft public
失败：保持 draft；已 public 则尽量 re-draft/delete + 公告；禁止仅 prerelease
```

---

## 分阶段最优路径

### Phase 0 — 第 1 天止血（不引入 reusable，先消假绿/漂移）

目标：修已确认事故；改 skip/阈值只动一处；dry-run 真能跑构建。

#### Task 1: 抽出 Miri / Coverage / Audit 脚本并两边共用

**Files:**
- Create: `scripts/ci/miri.sh`
- Create: `scripts/ci/coverage.sh`
- Create: `scripts/ci/audit.sh`
- Modify: `.github/workflows/ci.yml`（miri / coverage / cargo-audit steps）
- Modify: `.github/workflows/release.yml`（对应 steps）

- [ ] **Step 1: `scripts/ci/miri.sh`**

```bash
#!/usr/bin/env bash
set -euo pipefail
export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-nightly}"
cargo miri setup
cargo miri test -p tachyon-core --lib -- \
  --skip test_validate_save_path \
  --skip test_validate_multi_save_paths \
  --skip proptests
# dir_sync 与平台 IO 同列为 isolation skip（CI/Release 必须同源）
cargo miri test -p tachyon-io --lib -- \
  --skip iocp --skip iouring --skip pipeline \
  --skip tokio_file --skip winio --skip write_pipeline \
  --skip dir_sync
```

- [ ] **Step 2: `scripts/ci/coverage.sh`（先保持逐 crate 语义，后续再单次收集）**

```bash
#!/usr/bin/env bash
set -euo pipefail
IGNORE='(test_harness|iocp|winio|iouring)'
CRATES=(tachyon-core tachyon-engine tachyon-store tachyon-io tachyon-crypto tachyon-scheduler)
for crate in "${CRATES[@]}"; do
  echo "::group::覆盖率: $crate"
  cargo llvm-cov -p "$crate" --locked \
    --ignore-filename-regex "$IGNORE" \
    --fail-under-regions 90 --summary-only
  echo "::endgroup::"
done
if [[ "${COVERAGE_HTML:-0}" == "1" ]]; then
  cargo llvm-cov -p "${CRATES[@]}" --locked \
    --ignore-filename-regex "$IGNORE" --html || true
fi
```

- [ ] **Step 3: `scripts/ci/audit.sh`（ignore 单源）**

从 `deny.toml` 解析 `RUSTSEC-...` 生成 `cargo audit --ignore ...`，再 `cargo deny check`。禁止在 yml 手写第三份列表。

- [ ] **Step 4: ci.yml / release.yml 改为 `bash scripts/ci/...`**

- [ ] **Step 5: 验收**

```bash
# 两边 miri 命令 diff 为空（都调用同一脚本）
rg -n "dir_sync|scripts/ci/miri" .github/workflows/
# 期望：ci/release 都只出现 scripts/ci/miri.sh，release 不再缺 dir_sync
```

#### Task 2: 接通 dry-run + 修复 version-check

**Files:**
- Create: `scripts/ci/version-check.sh`
- Modify: `.github/workflows/release.yml`（version-check / publish / rollback / build 条件）

- [ ] **Step 1: version-check 双模式**

```bash
#!/usr/bin/env bash
set -euo pipefail
mode="${1:-auto}" # tag | files | auto
cargo_v=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\([^"]*\)".*/\1/')
tauri_v=$(grep -m1 '"version"' crates/tachyon-app/tauri.conf.json | sed 's/.*"\([^"]*\)".*/\1/')
fe_v=$(grep -m1 '"version"' frontend/package.json | sed 's/.*"\([^"]*\)".*/\1/')
[[ "$cargo_v" == "$tauri_v" && "$cargo_v" == "$fe_v" ]] || {
  echo "::error::版本互不一致 cargo=$cargo_v tauri=$tauri_v frontend=$fe_v"; exit 1; }
if [[ "$mode" == "tag" || ( "$mode" == "auto" && "${GITHUB_REF:-}" == refs/tags/v* ) ]]; then
  tag_v="${GITHUB_REF_NAME#v}"
  [[ "$tag_v" == "$cargo_v" ]] || { echo "::error::tag=$tag_v != cargo=$cargo_v"; exit 1; }
fi
echo "version ok: $cargo_v (mode=$mode)"
```

- [ ] **Step 2: dry_run 表达式**

```yaml
env:
  DRY_RUN: ${{ github.event_name == 'workflow_dispatch' && inputs.dry-run != 'false' }}

publish-release:
  if: success() && env.DRY_RUN != 'true'
  # 或: success() && (github.event_name == 'push' || inputs.dry-run == 'false')

rollback-on-failure:
  if: failure() && env.DRY_RUN != 'true'
```

- [ ] **Step 3: dry-run 时 build 不创建 GitHub Release**

tauri-action 在 dry-run 下应避免污染 draft（`releaseDraft` / upload 条件化，或 dry-run 只用 `uploadWorkflowArtifacts`）。

- [ ] **Step 4: 验收**

手动 `workflow_dispatch dry-run=true`：version-check 过、构建可跑、无 public release。

#### Task 3: publish 递归签名硬断言

**Files:**
- Modify: `.github/workflows/release.yml` `publish-release`

- [ ] **Step 1: 与 smoke 同一套找包**

```bash
mapfile -t FILES < <(find dist -type f \( \
  -name '*.msi' -o -name '*.deb' -o -name '*.dmg' -o -name '*.AppImage' \))
[[ ${#FILES[@]} -ge 1 ]] || { echo "::error::无产物可签名"; exit 1; }
for f in "${FILES[@]}"; do
  sha256sum "$f" | tee "$f.sha256"
  cosign sign-blob --yes --bundle "$f.bundle" "$f"
done
# 硬断言数量
[[ $(find dist -name '*.sha256' | wc -l) -eq ${#FILES[@]} ]]
[[ $(find dist -name '*.bundle' | wc -l) -eq ${#FILES[@]} ]]
```

- [ ] **Step 2（推荐同 PR）: 对 Release 资产再对账**

`gh release download "$TAG" --dir release-assets --pattern '*'` 后对 release 资产签名/哈希，避免 artifact 布局与用户下载物不一致。

#### Task 4: 文档止血 + frozen lockfile + fuzz key

**Files:**
- Modify: `CLAUDE.md`（覆盖率命令 → 与 AGENTS/CI 一致，或改指向脚本）
- Modify: `.github/workflows/ci.yml` / `release.yml` frontend steps
- Modify: `.github/workflows/fuzz.yml`

- [ ] CLAUDE.md 删除 `--fail-under-lines 90` 合计命令，改为：

```bash
bash scripts/ci/coverage.sh
# 或与 AGENTS.md 相同的 for 循环 + regions 90
```

- [ ] 所有 `bun install` → `bun install --frozen-lockfile`
- [ ] fuzz cache:

```yaml
- id: date
  run: echo "day=$(date -u +%Y%m%d)" >> "$GITHUB_OUTPUT"
- uses: actions/cache@v4
  with:
    path: fuzz/corpus
    key: fuzz-corpus-${{ steps.date.outputs.day }}
    restore-keys: |
      fuzz-corpus-
```

**Phase 0 验收清单**

- [ ] 改 miri skip 只改 `scripts/ci/miri.sh`
- [ ] dispatch dry-run=true 不再死于 version-check
- [ ] publish 无产物时硬失败，不再空签 public
- [ ] CLAUDE/AGENTS/CI 覆盖率口径一致
- [ ] bun lock 冻结

---

### Phase 1 — 第 1 周结构收敛

#### Task 5: reusable `ci-core.yml`

**Files:**
- Create: `.github/workflows/ci-core.yml` (`workflow_call`)
- Modify: `.github/workflows/ci.yml` → 薄触发器
- Modify: `.github/workflows/release.yml` → 删除内嵌 ci-gate/security-gate/frontend-gate，改为 call 或校验同 SHA ci-pass

推荐 release 策略（安全+成本折中）：

```text
tag → 校验同 SHA 的 ci-pass success（age < N 天）
    → 失败/缺失才 fallback 全量 ci-core
    → version-check → build → smoke → sign → publish
```

禁止：只看“main 最近一次绿”而不比 SHA。

#### Task 6: composite 环境 + cache shared-key

**Files:**
- Create: `.github/actions/setup-ubuntu-deps/action.yml`
- Create: `.github/actions/setup-rust/action.yml`（含 shared-key 约定）
- Create: `.github/actions/setup-bun/action.yml`

shared-key 示例：

- `ubuntu-stable-${{ hashFiles('**/Cargo.lock') }}` → clippy / test-ubuntu / docs  
- nightly / msrv 分 key，禁止跨 toolchain 强行共用

#### Task 7: path filters + bench 移出关键路径

- frontend-only → 只跑 frontend  
- docs / `.claude` only → 跳过重 job  
- `bench`：main 采样保留，**不**进 `ci-pass` needs（或 needs 但允许 skipped 且文档写明 bench 非门禁）

#### Task 8: coverage 单次收集 + 分 crate 断言

```bash
# 一次 instrument
cargo llvm-cov --json --summary-only -p ... --locked --ignore-filename-regex '...'
# jq 对 6 crate 分别 assert regions >= 90
# HTML 从同一产物生成
```

门禁语义不变，billable 预计明显下降（成本 Agent：cov 热 3.5→1–1.5 min）。

#### Task 9: 发布安全加固（与结构收敛并行）

1. 生成真实 Tauri ed25519；conf 禁止 PLACEHOLDER；缺私钥或无 `.sig` → build 失败  
2. `publish-release` 使用 Environment `release-production` + required reviewers  
3. job 级权限：默认 read；仅 build/publish/rollback write；仅 publish `id-token: write`  
4. 修 `verify-signature-config.sh`：PLACEHOLDER 必须 FAIL；endpoints 用可靠解析；接入 security-gate  
5. 真回滚：失败且已 public → 尽量 re-draft 或 delete + 公告  

#### Task 10: `scripts/ci/preflight.sh`

```bash
# --quick: fmt clippy nextest deny audit taplo doc
# --full:  + coverage + frontend（miri 可选）
```

`CLAUDE.md` / `AGENTS.md` 本地预检改为一行：

```bash
bash scripts/ci/preflight.sh --quick
```

**Phase 1 验收**

- [ ] Release 门禁强度 ≥ PR CI（同 SHA 或同 ci-core）  
- [ ] 无内嵌第二套 miri/coverage/audit 字面量  
- [ ] 纯前端 PR billable 显著下降  
- [ ] PLACEHOLDER 无法通过签名配置检查  

---

### Phase 2 — 以后防再漂

1. `ci/policy.toml`：crates、regions 阈值、miri skips、audit ignores  
2. `scripts/ci/check-doc-drift.sh`：禁止 docs 出现 `--fail-under-lines`；禁止 workflow 内联 `cargo llvm-cov`/`cargo miri test`  
3. SBOM（cargo + bun）+ cosign  
4. smoke 增加 cosign verify-blob + `.sig` 存在性  
5. tag 保护：签名 tag / 仅 main 受保护提交  
6. fuzz 复用 setup-ubuntu-deps  

---

## ROI 与落地顺序（不降门禁）

| 优先级 | 项 | 成本 | 收益 | 降门禁？ |
|--------|----|------|------|----------|
| 1 | miri/coverage/audit 脚本 SSOT + dir_sync 对齐 | S | 消漂移事故 | 否 |
| 2 | dry-run + version-check 双模式 | S | 发布链路可用 | 否 |
| 3 | 递归签名硬断言 + 资产对账 | S | 消供应链假绿 | **加强** |
| 4 | CLAUDE regions + frozen-lockfile + fuzz key | S | 假同源/可复现/cache | 否/加强 |
| 5 | coverage 单次 + 分 crate assert | S | billable −15–25% | 否 |
| 6 | release 绑同 SHA ci-pass / call ci-core | S–M | 每次 release 少 15–40 min 且更严 | 否/加强 |
| 7 | composite + shared-key cache | S | 冷启动 −20–40% | 否 |
| 8 | path filters | S | 纯前端 PR −70–90% | 条件否 |
| 9 | Tauri 真密钥 + Environment 审批 | M | 发布可信 | **加强** |
| 10 | bench 移出 ci-pass | S | main wall 视情况 −0–50% | 否（bench 本非正确性） |

热缓存 PR 预期：wall 仍可能由 Windows test 钉在 ~7 min；billable 从 ~35 min 压到 ~18–22 min 量级（成本 Agent 估算）。

---

## 文件布局（目标）

```text
scripts/ci/
  preflight.sh
  coverage.sh
  miri.sh
  audit.sh
  frontend.sh
  version-check.sh
  check-doc-drift.sh          # 后期
ci/
  policy.toml                 # 后期
.github/
  actions/
    setup-ubuntu-deps/action.yml
    setup-rust/action.yml
    setup-bun/action.yml
  workflows/
    ci-core.yml               # reusable
    ci.yml                    # 薄
    release.yml               # 薄 + 发布副作用
    fuzz.yml
  scripts/
    verify-signature-config.sh  # 修假阳性后接入
```

---

## ADR（短）

### ADR-CI-001：检查逻辑下沉脚本
- **决策：** 本地也要跑的门禁 SSOT 在 `scripts/ci/*.sh`
- **备选：** 只抽 composite / 只抽 reusable（本地仍不同源）
- **后果：** 多一层文件；改 skip/阈值一处生效

### ADR-CI-002：Release 调用或绑定完整 CI，禁止内嵌精简 CI
- **决策：** `ci-core` 或同 SHA `ci-pass`；发布特有逻辑留 release
- **后果：** 删除双份 YAML；Release 更诚实，可能略慢（fallback 时）

### ADR-CI-003：覆盖率口径 regions + 逐 crate
- **决策：** 永久废弃 lines 合计文案
- **后果：** 文档统一；禁止 fail-under-lines

### ADR-CI-004：dry-run 是控制流变量
- **决策：** 显式表达式驱动 publish/rollback/release 创建
- **后果：** 可安全反复验证构建

### ADR-CI-005：签名对用户下载物强制且可数
- **决策：** 递归找包 + 数量硬断言 +（推荐）Release 资产对账；Tauri 真密钥
- **后果：** 无签名不可 public

### ADR-CI-006：文档只引用入口
- **决策：** `preflight.sh` 一行；禁止复制 80 字符命令串当 SSOT
- **后果：** 漂移面收敛

---

## 本轮不做（YAGNI）

- 自建 CI 框架 / Makefile 第四份命令封装  
- sccache 远程缓存（运维重，ROI 中）  
- 自托管大 runner（视预算）  
- 把 bench 变成硬阈值  
- 为省时间跳过 `--locked`  

---

## 实现任务勾选总表

### Phase 0
- [ ] Task 1: miri/coverage/audit 脚本 SSOT
- [ ] Task 2: dry-run + version-check
- [ ] Task 3: publish 递归签名硬断言
- [ ] Task 4: 文档 / frozen-lockfile / fuzz key

### Phase 1
- [ ] Task 5: ci-core reusable + release 绑定
- [ ] Task 6: composite + shared-key
- [ ] Task 7: path filters + bench 非关键
- [ ] Task 8: coverage 单次收集
- [ ] Task 9: Tauri 真签 + Environment + 权限收紧 + 脚本修假阳性
- [ ] Task 10: preflight 入口

### Phase 2
- [ ] policy.toml + doc-drift job + SBOM + smoke verify + tag 保护
