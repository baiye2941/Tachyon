# Evidence bundle (in progress)

| Evidence | Status |
|---|---|
| Source baseline | `58bc93923fe93031330589c96afc22ac55c23a6c`, clean isolated worktree |
| Main worktree safety | Dirty user changes observed; no edits made there |
| Audit report | Local untracked document read at absolute source path |
| Claim verification | Pending independent auditors |

## Fresh baseline commands

| Command | Result | Scope |
|---|---|---|
| `cargo fmt --all -- --check` | exit 0 | workspace formatting |
| `cargo clippy --all-targets --all-features --locked -- -D warnings` | exit 0 | host platform/all features |
| `cargo nextest run --all` | exit 0; 1903 passed | workspace tests on current Windows host |
| `cd frontend && bun install --frozen-lockfile` | exit 0 | frontend locked dependencies |
| `cd frontend && bun run typecheck` | exit 0 | frontend TypeScript |
| `cd frontend && bun run lint` | exit 0 | frontend lint, zero warnings |
| `cd frontend && bun run test` | exit 0; 74 files / 826 tests | frontend Vitest |
| `cd frontend && bun run build` | exit 0 | frontend production build |

## Independent audit synthesis

| Domain | Independent result | Key correction |
|---|---|---|
| Security/correctness | S-01/S-02 confirmed; S-03/S-04 partial | S-04 `task_service` claim is stale; S-03 needs release invariant design, not debug_assert only |
| Performance | P-01 serial confirmed; P-03 limited production non-reachability confirmed | HLS concurrency needs ordered bounded prefetch design; project performance numbers not proven |
| CI/quality | E-01/E-02 confirmed; E-03/E-04/E-05/E-06/E-09 qualified | Miri exists; retries show risk rather than present reproduction; dead-code count is 10, not 13 |
| Red team | report not entirely correct | self-reference path stale; many statements need evidence-bound wording |

## Design review history

- First independent design review rejected the initial scope due to legacy peer-config migration, all-cache-miss peer input coverage, future-schema destructive workflow gaps, and S-04 ordering/testability gaps.
- User resolved the three required policy choices.
- Revised design/spec and plan now include those boundaries; independent re-review remains pending.

## Second design review evidence

| Review | Blocking finding | Consequence |
|---|---|---|
| S-01/S-02 security review | future schema restore/delete/export/import have unaddressed data-loss paths | S-02 cannot use a minimal header-only patch |
| S-04 review | hook must execute inside blocking closure; seed-state and topology state require distinct ordering contract | S-04 spec revised to use test-only RAII hook and explicit barrier |
| plan feasibility review | full fail-closed requires a proven linearization boundary | S-02 follow-up design created; implementation paused |

## User-approved execution boundary

| Decision | Scope | Evidence / implication |
|---|---|---|
| Execute S-04 and S-01 first | strict TDD | User explicitly selected “先做 S-04+S-01” on 2026-07-22 |
| S-02 process-local strict protection | future snapshot persistence | User selected reservation + admission gate + compensation, without crash-transaction claim |
| S-04 confirmed candidate | `chunk_reader_pool.rs` only | independent verification: synchronous TaskStore load in async PlanComplete branch; `task_service` prior allegation is stale |

## S-04 RED evidence

| Test | Command shape | Result | Why it is valid RED |
|---|---|---|---|
| PlanComplete barrier | `cargo nextest run -p tachyon-app --no-default-features --lib plan_complete_waits_for_blocking_snapshot_before_callbacks_and_chunks -- --nocapture` | exit 100; callback happened before hook start | current direct synchronous load bypasses new blocking-closure hook |
| PlanComplete JoinError fallback | `cargo nextest run -p tachyon-app --no-default-features --lib plan_complete_falls_back_to_repository_when_snapshot_blocking_task_panics -- --nocapture` | exit 100; callback happened before hook start | current production has no spawned closure / JoinError path |

Tester diff was inspected: all additions begin after `#[cfg(test)] mod tests`; `git diff --check` was clean.

## S-04 GREEN / independent review evidence

| Evidence | Result |
|---|---|
| Exact PlanComplete barrier test under `--no-default-features` | pass |
| Exact JoinError fallback test under `--no-default-features` | pass |
| `runtime::chunk_reader_pool` regression | 16/16 pass |
| `cargo fmt --all -- --check` | pass after Tester-only rustfmt correction |
| `git diff --check` | pass |
| Independent code/spec reviewer | approve; no high/medium findings |
| `cargo nextest run -p tachyon-app --no-default-features --lib` | unrelated baseline/feature-matrix failure: magnet acceptance test runs with magnet disabled |
| default-feature build/test | environment blocked: aws-lc-sys requires NASM |

S-02 reviewer finding was incorporated only into the design document; implementation remains intentionally unstarted.

## S-01A TDD evidence

| Step | Evidence |
|---|---|
| RED | Tester observed missing `is_restricted_peer_ip`, `MagnetConfig.allow_private_peers`, and `MagnetPatch.allow_private_peers` compilation failures before production implementation. |
| GREEN targeted | Three test-harness target tests passed using discovered nextest test IDs. |
| HTTP compatibility | Existing `rejects_non_loopback_private_ips_under_test_harness` passed; review remediation added and passed `::ffff:127.0.0.1` loopback allowance. |
| Core regression | `cargo nextest run -p tachyon-core --lib --features test-harness`: 486 passed; no-feature core lib: 486 passed. |
| Quality | `cargo fmt --all -- --check`, `cargo clippy -p tachyon-core --all-targets --all-features -- -D warnings`, `git diff --check` passed. |

The first independent reviewer requested only scope/coverage/spec-alignment closure; re-review is pending.

## S-01B TDD / verification evidence

| Stage | Evidence |
|---|---|
| Behavior RED | Test-local legacy unfiltered merge violated default filtering, opt-in, quota/cap and backfill assertions. |
| Interface RED | Frozen tests failed on absent `collect_initial_peers`. |
| Review RED | `magnet:é` reproduced a UTF-8 byte-boundary panic at legacy parser offset 8. |
| GREEN | Private collector uses `url.get(8..).is_some()`, core predicate, source-local stable dedup, URI cross-source precedence and 32/32/64 ordering. |
| Target tests | 5 collector tests passed (four policy/order tests plus malformed URI boundary). |
| Regression | `cargo nextest run -p tachyon-protocol --features test-harness`: 234 passed; default protocol suite: 234 passed. |
| Quality | protocol all-feature check + clippy `-D warnings`, format check and diff check passed. |
| Review | Path-scoped re-review approved S01-B; no S01B-induced blocker remains. |

## Deferred baseline SessionOpsGate finding

| Item | Evidence | Scope decision |
|---|---|---|
| add/cleanup gate key differs after SOCKS UDP tracker strip | present in `58bc939` and unchanged by S01-B | do not silently expand S01-B; user scope confirmation required for a dedicated concurrency slice |

## Claim reconciliation updates

| Area | Corrected evidence-backed classification |
|---|---|
| S-04 | confirmed and locally accepted; only `chunk_reader_pool.rs` sync snapshot path was established |
| S-01A/S-01B | confirmed and locally accepted with core/protocol target plus regression evidence |
| S-01C | required and not yet implemented at this checkpoint |
| S-02 | confirmed risk; revised S-02a design exists but has no fresh reviewer/user execution approval |
| E-01 | mutants absent from CI; design budget/threshold required before a gate |
| E-02 | Windows lacks native Clippy/MSRV, but runs nextest |
| E-03 | missing unsafe-comment lint; Miri exists |
| E-04 | retry masking risk, not fresh flaky reproduction |
| E-07 | six benches; architecture CI table omits msrv/doc-drift |
| E-08/E-09 | `navigator.platform` five sites; `allow(dead_code)` ten mixed-purpose sites |

## S-01C Tester RED

| Item | Evidence |
|---|---|
| Test owner | `frontend/src/components/__tests__/SettingsPanel.spec.tsx` only |
| Added cases | legacy missing DTO defaults toggle false; controlled opt-in toggle saves patch and displays all risk statements |
| Exact RED | `cd frontend && bun run vitest run src/components/__tests__/SettingsPanel.spec.tsx -t "后端旧 DTO 缺 allowPrivatePeers 时受限 Peer 开关默认关闭|允许私有 Peer 开关受控翻转并保存补丁，显示完整风险提示"` |
| Result | 2 failed, 27 skipped; both failed because `允许私有 Peer` was absent from current MagnetTab DOM |
| Validity | production contract/UI absence; no production code was edited by Tester |

## S-01C first independent acceptance / review

| Check | Result |
|---|---|
| Target accessibility/config tests | passed, 2 selected cases |
| Full Vitest | passed, 74 files / 828 tests |
| Diff check | passed |
| Acceptance outcome | rejected: imperative per-instance aria mutation and inaccessible button name |
| Required remediation | role/name-based Tester RED; canonical `ToggleItem` declarative a11y; remove MagnetTab workaround |

## S-01C accessibility remediation

| Stage | Evidence |
|---|---|
| Reviewer finding | imperative MagnetTab DOM mutation + structural test coupling blocked acceptance |
| New RED | role/name selector tests failed before canonical ToggleItem accessible name existed |
| GREEN | ToggleItem owns declarative `type`, `aria-label`, `aria-pressed`; MagnetTab workaround removed |
| Secondary diagnostic | new jest-dom matcher type declarations absent in project TS config |
| Minimal repair | Tester only replaced three new matcher calls with native `getAttribute`; no dependency/setup expansion |
| Local verification | two target tests passed; SettingsPanel file 29 passed; frontend typecheck passed; diff check passed |

## S-01C final acceptance evidence

| Check | Result |
|---|---|
| Canonical a11y ownership | `ToggleItem` declaratively owns button type, accessible name and pressed state; MagnetTab imperative workaround removed |
| Rust/TS contract | Rust `rename_all = "camelCase"` + defaulted `allow_private_peers`; frontend optional `allowPrivatePeers` matches current and legacy DTO needs |
| Frontend lint | passed, zero warnings |
| Exact S01C tests | 2 selected passed |
| Typecheck | passed |
| SettingsPanel suite | 29 passed |
| Full frontend Vitest | 74 files / 828 tests passed |
| Diff check | passed |
| Independent review | final path-scoped APPROVE |

## Post-S01 independent preflight evidence

| Claim | Result | Safe next boundary |
|---|---|---|
| S-02a | design review request changes | resolve store/app startup and typed-error contract before user-authorized TDD |
| S-03 | partially confirmed: missing known-length fragmented final invariant | separate engine design; no broad Completed unification |
| S-05 | conditionally confirmed local FS race | cross-platform dirfd/HANDLE architecture design, not a local check |
| P-01 | serial fetching confirmed, gains unmeasured | bounded concurrent HLS design + real e2e benchmark |
| P-05 | Windows directory durability guarantee unproven | documentation/semantic design + fault evidence |
| E-01/E-02/E-04/E-07/E-08/E-09 | mixed governance/static confirmations | separate policy/doc/owner-specific slices |

## Combined workspace verification after S-01/S-04

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | passed |
| `cargo nextest run --all` | 1913 passed, 0 skipped, 0 failed |
| `cargo clippy --all-targets --all-features --locked -- -D warnings` | passed |
| `cargo build --all --locked` | passed |
| Frontend lint/typecheck/full Vitest | passed; 74 files / 828 tests |
| Independent final S-01 integration review | APPROVE |

## S-04 final-review remediation evidence (pending)

| Item | Status |
|---|---|
| Finding | global test hook was consumable by unrelated parallel PlanComplete jobs |
| Root cause | unqualified global `take()`; install lock did not cover unrelated consumers |
| Required repair | task-ID-directed test-only `take_for`, preserving RAII one-shot behavior |
| Required RED | nonmatching PlanComplete must not consume a target hook |
| Production scope | unchanged; no production abstraction/API expansion |

## S-04 test-hook remediation final evidence

| Stage | Evidence |
|---|---|
| Review finding | unqualified global test hook could be consumed by unrelated parallel PlanComplete |
| Valid RED | new nonmatching-task test failed at explicit “other task consumed target hook” assertion |
| GREEN | task-ID-directed `take_for`; unmatched jobs preserve slot and use actual store loader |
| Target app tests | isolation + barrier + JoinError passed; chunk_reader_pool app tests 17/17 passed |
| Quality | tachyon-app lib clippy `-D warnings`, rustfmt and diff check passed |
| Independent review | Tester APPROVE and Reviewer APPROVE |
| Fresh workspace regression | 1914 nextest tests passed; fmt/diff passed |
| Frontend production build | passed |
