# Checkpoint 1

## Current todo

1. Build a claim inventory from the audit report.
2. Independently validate security/correctness, performance, and engineering/CI claim groups in parallel.
3. Reconcile evidence and classify each actionable claim.
4. Write a bounded implementation plan only for confirmed, owned repairs.
5. Run Tester → Coder → Tester → Reviewer strict-TDD slices sequentially.
6. Run final cross-agent review and fresh verification.

## Active slice

Read-only claim verification at frozen `58bc939`.

## Explicit non-edits

No production, test, CI, or documentation edits until claim reconciliation,
canonical-owner analysis, and plan are complete.

## Verification

Worktree is clean at `58bc939`; main worktree was observed dirty and is isolated.

## Resume state

- Worktree: `C:/Users/白夜/.config/aegis/worktrees/Tachyon/audit-verify-20260722`
- Branch: `audit/verify-20260722`
- Report source: `D:/Rust/Tachyon/Document/Tachyon系统性深度审计报告-2026-07-22.md`
- Next: dispatch independent read-only auditors with disjoint claim domains.

## Checkpoint 2 — independent claim reconciliation complete

### Completed

- Read-only independent security/correctness, performance, engineering/CI, red-team, and claim-inventory reviews completed.
- Fresh baseline verification completed: `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, `cargo nextest run --all` (1903/1903), frontend typecheck/lint/Vitest/build (74 files / 826 tests).
- User chose: default reject plus explicit private-peer opt-in; future schemas fully fail-closed; deterministic S-04 test seam.
- First design review exposed material gaps; design was revised. Independent re-review is running before user approval is requested.

### Current todo

1. Collect independent re-review of revised design and plan.
2. Reconcile any findings into design/plan.
3. Request explicit user review/approval of the written design before source edits.
4. Execute S-01/S-02/S-04 sequential strict-TDD lanes only after approval.

### Evidence refs

- `10-intent.md`
- `90-evidence.md`
- `../specs/2026-07-22-audit-s01-s02-s04-design.md`
- `../plans/2026-07-22-audit-s01-s02-s04-implementation.md`
- `/tmp/tachyon-audit-baseline-nextest.log`
- `/tmp/tachyon-audit-baseline-fmt.log`
- `/tmp/tachyon-audit-baseline-clippy.log`
- `/tmp/tachyon-audit-frontend-{typecheck,lint,test,build}.log`

### Drift check

- Original intent: still aligned; correction work remained bounded to audit-verified claims.
- Compatibility: elevated and explicitly resolved by user decisions; final design awaiting re-review.
- New owners/fallbacks: none approved; design explicitly forbids them.
- Decision: needs-verification.

## Checkpoint 3 — S-02 architecture escalation

### New evidence

The second independent design review rejected direct S-02 execution:

- Restore requires durable-write-before-tombstone-removal, not merely guard-before-removal.
- Duplicate `schemaVersion` requires streaming parsing, not `serde_json::Value`.
- Old `recover_pending_tasks` and app destructive flows need explicit semantics.
- `TaskService::delete_task` and import currently have error swallowing / mutate-before-save paths.
- A scan-before-mutation design would leave a TOCTOU gap under the user's full fail-closed policy.

### Decision

- S-02 is split into `docs/aegis/specs/2026-07-22-s02-full-fail-closed-followup.md`.
- No S-02 source edit may begin until a concrete store/app linearization or reservation/compensation design is independently approved.
- S-01/S-04 remain separately under re-review; neither has started source edits.

### Drift check

- Scope: audit-confirmed fixes remain the goal.
- Compatibility: S-02 has escalated rather than accepting an unsafe local patch.
- Retirement: no deletion or persistent-state mutation performed.
- Decision: blocked on S-02 architecture design; S-01/S-04 verification pending.

## Checkpoint 4 — user-approved execution boundary

### User decisions

- Approved: first execute S-04 and S-01 under strict TDD and independent review.
- Approved S-02 target: process-local strict protection using a store reservation, app admission gate and explicit compensation; no claim of crash-atomic multi-resource transaction.

### Active slices

- S-04: Tester has been assigned the deterministic RED infrastructure/tests in the test-only region of `chunk_reader_pool.rs`. Coder has not started.
- S-01: a read-only preflight is in progress to pin actual test, IPC and frontend boundaries before RED.
- S-02: an independent architecture review is in progress; no implementation starts until it approves an exact API and lock model.

### TDD record

```text
TDD Route:
- Mode: auto
- Decision: strict
- Reason: confirmed concurrency scheduling behavior and security/config contract changes.
- Verification: Tester RED -> Coder GREEN -> Tester regression -> independent Reviewer.

Change Necessity (S-04):
- User-visible need: PlanComplete must not synchronously block a Tokio worker.
- No-change / non-code option: report correction cannot remove the synchronous filesystem read.
- Why code change is necessary: the source directly calls the synchronous TaskStore API within an async worker.
- Minimum change boundary: one private loader helper and PlanComplete wiring in chunk_reader_pool.rs.
- Decision: code-change.

Complexity Budget (S-04):
- Artifact class: already-large runtime state-machine module.
- Current pressure: ~1.8k lines, progress and persistence logic coupled.
- Projected post-change pressure: one private helper plus test-only hook only.
- Planned governance: no generic executor; no TaskStore API change; production/test regions split by role.
```

## Checkpoint 5 — S-04 RED accepted; S-01 preflight complete

### S-04 Tester handoff

- Tester-only diff adds deterministic hook/tests exclusively inside `chunk_reader_pool.rs` `#[cfg(test)] mod tests`.
- Exact behavior RED was observed under `--no-default-features`:
  - `plan_complete_waits_for_blocking_snapshot_before_callbacks_and_chunks`
  - `plan_complete_falls_back_to_repository_when_snapshot_blocking_task_panics`
- Both failed because current production invokes callback before the hook's blocking closure starts. This is an intended behavior RED, not a compilation failure.
- Default feature test build is blocked by environmental `aws-lc-sys` NASM absence; this is not treated as a product failure.
- Coder assigned only production region of the same file; tests remain frozen.

### S-01 preflight result

- S01-A (core config + peer policy), S01-B (protocol collector), S01-C (app/frontend wiring) have an exact owner/read-set/slice card from independent read-only review.
- S01 execution remains sequenced behind S-04 review, per strict Tester → Coder → Tester → Reviewer workflow.

## Checkpoint 6 — S-04 accepted; S-02 approved design refined

### S-04 strict TDD closure

- Tester observed both behavior REDs before production code existed.
- Coder added the one private `spawn_blocking` loader and awaited PlanComplete wiring only.
- Independent Tester acceptance:
  - formatting and diff checks passed;
  - both exact S-04 tests passed;
  - all 16 `runtime::chunk_reader_pool` no-default-feature tests passed.
- Independent Reviewer approved spec compliance with no blocking/high/medium findings.
- Full `tachyon-app --no-default-features --lib` remains red only at the pre-existing feature-matrix mismatch `commands::tests::test_validate_download_url_accepts_magnet` (magnet disabled under no-default-features); it is unrelated to S-04. Default feature validation remains blocked by missing NASM for aws-lc-sys.

### S-02 design refinement

- Independent architecture reviewer initially rejected the concept-only follow-up.
- The follow-up spec now explicitly pins reservation capability, TaskStore-owned admission gate, fixed lock rules, RawValue import envelope, strict durable restore/revision order, all load/write consumer paths, and non-transactional compensation boundary.
- No S-02 source or test implementation has begun. A fresh independent review is required before S-02 Tester RED.

### Next active work

- S-01-A can begin its Tester RED only after an independent slice-card/spec review records the chosen core predicate name and test names.
- S-04 is locally accepted but not release-complete until broader environment-supported validation occurs.

## Checkpoint 7 — S-01A strict TDD evidence; review remediation pending re-review

### S-01A route and evidence

- The S-01A contract was refined to an exact core predicate, config field, test IDs and valid nextest syntax before Tester RED.
- Tester created the core tests only and observed compilation RED for the absent predicate/config fields.
- Coder implemented only core safety/config/re-export production paths; Tester repaired affected pre-existing test literals only.
- Core evidence observed by independent tester/controller:
  - three targeted test-harness tests pass;
  - existing HTTP test-harness loopback compatibility test passes;
  - core `--features test-harness` regression passed (486 tests in controller run);
  - core default lib regression passed (486 tests);
  - `cargo fmt --all -- --check`, core all-feature clippy `-D warnings`, and `git diff --check` pass.

### Review closure

- First Reviewer confirmed implementation semantics but requested: path-scoped review due to parallel S-04 diff, test-ID spec alignment, and IPv4-mapped loopback regression coverage.
- Test IDs/spec syntax were aligned; the mapped-loopback regression was added as a coverage-only assertion and passed.
- A path-scoped independent re-review has been dispatched. No S-01B code starts until it approves.

### S-02 refinement

- A second S-02 review found the giant first slice overreached app outcomes/lifecycle. Follow-up design now divides pure store S-02a from app facade/startup S-02a2 and blocks runtime cancellation propagation behind an explicit read-set/design slice.
- No S-02 source implementation has started.

## Checkpoint 8 — S-01B strict TDD closure; baseline gate finding isolated

### S-01B strict TDD closure

- Tester wrote four offline collector tests and observed behavior RED against a test-local model of the legacy unfiltered merge, then compiler RED for the absent private collector.
- Coder implemented the module-private collector and rewired exactly the four cache-miss add paths; Tester assets stayed test-only.
- An independent review found a malformed UTF-8 byte-boundary panic in the new collector. A separate Tester added a failing `catch_unwind` regression; Coder changed only the collector guard from byte length to `url.get(8..).is_some()`; the five collector tests then passed.
- Controller verification:
  - five target collector/boundary tests passed under `tachyon-protocol --features test-harness`;
  - `cargo check -p tachyon-protocol --all-features` passed;
  - `cargo clippy -p tachyon-protocol --all-targets --all-features -- -D warnings` passed;
  - both protocol test-harness and default suites passed, 234/234 each;
  - `cargo fmt --all -- --check` and `git diff --check` passed.
- Path-scoped reviewer approved S01-B after excluding an independent baseline issue below.

### New baseline finding: SessionOpsGate identity mismatch

- Evidence: both frozen baseline `58bc939` and current code use raw magnet URL for `stop_and_remove_torrent`, but use UDP-tracker-stripped `url_owned` as the add operation gate key under SOCKS.
- Trigger: SOCKS enabled + magnet contains a UDP tracker + cleanup/add overlap; the two operations can use different gate keys.
- Classification: real but **pre-existing, non-blocking for S01-B**; S01-B does not modify gate URL identity, SOCKS stripping, or cleanup/add sequencing.
- Safe scope: do not fold it into the approved peer collector slice. Preserve evidence and await explicit user choice to create a separate session-operation identity/concurrency slice with dedicated TDD tests.

### Next work

- S-01C (existing config transaction and frontend DTO/UI wiring) may proceed through read-only preflight because it is already in the approved S-01 scope.
- S-02 remains design-only; its revised slicing needs fresh approval before any Tester RED.

## Checkpoint 9 — report reconciliation, S-01C strict-TDD start, S-02 still gated

### Claim reconciliation

- Independent engineering/CI audit confirmed several static workflow facts and corrected overstated report wording: fuzz is scheduled/manual rather than a PR gate; Windows lacks native Clippy/MSRV gates but does run nextest; Miri exists, so E-03 is a missing unsafe-documentation lint rather than no machine check; retry is a masking risk rather than a newly reproduced failure; bench count is six; `navigator.platform` is five sites; and `#[allow(dead_code)]` is ten sites with mixed purposes.
- A fresh matrix records S-04, S-01A and S-01B as locally accepted/verified; S-01C remains required for the configuration/UI path; S-02 remains design-only; other audit items are classified as design/measurement/governance work rather than silently implemented.

### S-01C Slice Card

- Goal: complete only the existing `allow_private_peers` configuration/UI contract: frontend DTO, draft, patch, controlled toggle, bilingual risk guidance; reuse existing app config transaction without modifying its production behavior unless a regression test establishes a gap.
- Parent: `docs/aegis/specs/2026-07-22-audit-s01-s02-s04-design.md` §3.5/§3.6 and `docs/aegis/plans/2026-07-22-audit-s01-s02-s04-implementation.md` Slice 1.
- Tester allowlist: `frontend/src/components/__tests__/SettingsPanel.spec.tsx` only.
- Coder allowlist after valid RED: `frontend/src/types.ts`, `frontend/src/components/settings/SettingsPanel.tsx`, `frontend/src/components/settings/tabs/MagnetTab.tsx`, `frontend/src/i18n/locales/zh-CN.ts`, `frontend/src/i18n/locales/en-US.ts`; no core/protocol/runtime or unrelated frontend files.
- Explicit non-edit: `frontend/src/stores/taskActions.ts` is untracked/nonexistent in this worktree and is not S-01C scope.
- Tester has been dispatched to create the minimum front-end RED tests. Coder has not started.

### S-02 gate status

- S-02a has a bounded store-only design, but neither the revised follow-up design nor S-02a has a recorded fresh independent approval plus execution authorization.
- No S-02 source/test diff exists. No Tester RED may begin.
- Minimal next step, after S-01C closure: independent read-only architecture review of only S-02a; if approved, request explicit user authorization before TDD starts.

### Drift check

- Original intent remains served: audit claims are being corrected rather than blindly implemented.
- S-01C stays in approved configuration/UI compatibility boundary; no owner/fallback expansion.
- S-02 remains blocked rather than accepting an under-specified destructive-data change.
- Decision: continue S-01C Tester RED; S-02 needs independent design review and user approval.

## Checkpoint 10 — S-01C Tester RED accepted; Coder GREEN in progress

### Tester evidence

- Tester-only file: `frontend/src/components/__tests__/SettingsPanel.spec.tsx`.
- Fixture gained `allowPrivatePeers: false`; two tests cover legacy DTO default false and controlled toggle/save patch plus the four mandatory Chinese risk statements.
- Exact command produced a valid RED: 2 failed / 27 skipped because `MagnetTab` has no `允许私有 Peer` DOM element. This is a production behavior/UI absence, not a test setup failure.
- Tester asset freeze is in effect. Coder has the narrowly allowlisted five frontend production files only; Rust, docs, package files, `taskActions.ts`, and Tester tests are explicit non-edits.

### Active slice

- Coder implementing minimal frontend DTO/draft/patch/toggle/i18n GREEN path.
- Required Coder evidence: exact two-test GREEN, Bun typecheck, and `git diff --check`.

### Drift check

- The slice still only realizes the user-approved config UI contract; no security policy moves to frontend and no new owner is introduced.
- Decision: continue through Coder GREEN, then independent Tester regression and reviewer.

## Checkpoint 11 — S-01C review rejected; accessibility remediation enters new RED

### Independent acceptance/review result

- Target tests and full frontend Vitest passed (74 files / 828 tests), and diff check passed.
- Independent Tester and Reviewer both rejected the S-01C implementation on the same valid path-scoped issue: `MagnetTab` introduced an imperative `ref → querySelector → setAttribute('aria-pressed')` workaround. It depends on ToggleItem DOM shape and does not give the button an accessible name. The corresponding new tests also coupled to styling classes and DOM internals.
- This is a S01C-introduced quality/accessibility defect; it blocks acceptance despite functional tests passing.

### Approved narrow remediation boundary

- Allowlist expands only to the canonical shared owner `frontend/src/components/settings/items/ToggleItem.tsx`.
- Required direction: Tester first changes only the two new S01C tests to role/name-based accessibility assertions and obtains a RED because ToggleItem lacks an accessible name. Coder then removes the MagnetTab imperative workaround and adds declarative `type="button"`, accessible name, and `aria-pressed` at ToggleItem.
- No Rust, package, unrelated UI, protocol/core policy, or `taskActions.ts` work is authorized.

### Current active task

- New independent Tester is rewriting only Tester-owned S01C assertions and obtaining exact accessibility RED. Coder must not start until it returns.

### Drift check

- The remediation contracts behavior with the existing canonical UI primitive instead of creating a per-toggle workaround. It remains inside S01C’s configuration/UI boundary, adds no owner or fallback, and improves a11y rather than expanding product scope.
- Decision: continue strict TDD remediation.

## Checkpoint 12 — S-01C accessibility remediation GREEN; final independent gates running

### Remediation TDD evidence

- The first S01C implementation was rejected because it added an imperative per-instance DOM mutation. Independent review localized the canonical owner to `ToggleItem`.
- A new Tester changed only the two S01C tests to role/name selectors, producing RED from the existing toggle’s missing accessible name. Controller reran the correct frontend workspace command after the Tester initially used an incorrect root `bunx` invocation; the valid frontend command reproduced RED.
- Coder changed only canonical `ToggleItem` (`type`, `aria-label`, `aria-pressed`) and removed the MagnetTab ref/effect/querySelector workaround. The two a11y tests became GREEN.
- New test matcher syntax then exposed a new S01C TypeScript diagnostic. Independent diagnosis confirmed `toHaveAttribute` lacked configured jest-dom types; the smallest safe repair was test-only native `getAttribute`, not a dependency/setup/config expansion. Tester applied it and verified: two target tests, full SettingsPanel test file, frontend typecheck, and diff check all pass.

### Current gates

- Final independent Tester is running exact/full frontend tests, typecheck, lint, diff check, and Rust camelCase contract readback.
- Final independent Reviewer is doing a path-scoped S01C code review.
- S01C is not yet accepted until both return approve/pass.

### Scope / drift

- Remediation retired the new per-instance imperative workaround, consolidating accessible toggle semantics in the existing canonical shared primitive.
- No new frontend dependency/setup or policy owner was added. S01C remains within the approved UI/config boundary.
- Decision: needs-final-verification.

## Checkpoint 13 — S-01C accepted; S-01 end-to-end configuration boundary closed

### Final independent acceptance

- Independent code reviewer approved the final S01C scoped diff after the test assertion type-safety and lint fixture issues were repaired.
- Independent field-contract check confirmed Rust `MagnetConfig` uses `#[serde(rename_all = "camelCase")]` and `#[serde(default)] allow_private_peers`; frontend `allowPrivatePeers?: boolean` is therefore correct for current output plus historical DTO compatibility.
- Controller final frontend gates, run from the isolated worktree:
  - lint passed with zero warnings;
  - two precise S01C tests passed (2 selected);
  - typecheck passed;
  - SettingsPanel file passed (29 tests);
  - full Vitest passed (74 files / 828 tests);
  - `git diff --check` passed.
- S01C’s first imperative a11y workaround was retired. `ToggleItem` is the single shared owner of button type/name/pressed semantics; MagnetTab remains declarative.

### S-01 disposition

- S-01A (core classification/config), S-01B (protocol canonical collector), and S-01C (app/frontend configuration contract) are all locally accepted through strict Tester → Coder → Tester → Reviewer evidence.
- S-01 overall remains subject to eventual integration-level verification with all concurrent S-01/S-04 changes, but no S-01 slice-level blocker remains.

### Next work

- S-02 remains blocked: first obtain a fresh independent architecture review of only S-02a, then seek explicit user authorization before Tester RED.
- Other confirmed audit claims remain classified for separate design/measurement/governance slices; do not silently expand the completed S-01 boundary.

### Drift check

- Original audit-validation goal remains served. The only remediation expansion was to canonical shared ToggleItem ownership and eliminated a workaround rather than adding a branch/owner.
- Compatibility remains historical DTO/config default false; no peer policy was moved to frontend.
- Decision: S-01 accepted at slice level; S-02 needs approval; task continues under parent audit plan.

## Checkpoint 14 — post-S01 claim triage; no new implementation starts

### Fresh independent preflight outcomes

- **S-02a:** reviewer requested design changes before Tester RED. The pure-store scope conflicts with non-silent startup handling, and its typed error/API migration is under-specified. No S-02 edit begins. Required resolution: explicitly make S-02a an inseparable internal prerequisite to S-02a2, or expand the authorized unit; fix error enum/signature/legacy API contract; re-review then request user authorization.
- **S-03:** partially confirmed. The actual issue is missing a known-length fragmented-download task-level final structural invariant before `Completed`, not a debug-only guard or complete lack of release checks. A narrow engine slice may be designed later; it must exclude unknown-length, managed BT piece truth, fallback and existing single-stream semantics.
- **S-05:** confirmed as a conditional local filesystem race. Current path validation plus final-component defenses do not bind intermediate directory identity through open; advanced IO strategies have inconsistent final-component defenses. Full remedy requires a cross-platform handle/dirfd design, not a check/retry patch.
- **P-01:** confirmed serial HLS segment fetching, but no project benchmark establishes a throughput percentage. Concurrent HLS needs a bounded ordering/cancellation/rate/key-cache/memory design plus a real `DownloadTask::run()` benchmark first.
- **P-05/E-01/E-02/E-04/E-07/E-08/E-09:** governance review confirms their static facts with prior report wording corrections. Only E-07 factual documentation reconciliation appears potentially low-risk after SSOT readback; CI/persistence/platform changes require separate policy/design evidence.

### Active boundary

- No source edits are authorized beyond already accepted S-01/S-04 work.
- No S-02 Tester RED begins.
- Next user-facing decision should choose a bounded next authorized work unit rather than silently expanding from the audit report.

### Drift check

- The audit goal remains intact: only confirmed, properly owned claims are advanced.
- New reviews narrowed/qualified claims instead of creating speculative fixes.
- Decision: pause-for-user for next implementation priority and S-02 design scope decision.

## Checkpoint 15 — combined S-01/S-04 workspace verification and final S-01 integration review

### Combined commands

Fresh runs in the isolated worktree after all accepted S-01/S-04 diffs:

- `cargo fmt --all -- --check` passed.
- `cargo nextest run --all` passed: 1913 passed, 0 skipped, 0 failed.
- `cargo clippy --all-targets --all-features --locked -- -D warnings` passed.
- `cargo build --all --locked` passed.
- Frontend checks remain fresh from S-01C: lint, typecheck, SettingsPanel 29 tests, full Vitest 74 files / 828 tests, and diff check all passed.

### Independent integration review

- Final cross-layer S-01 reviewer approved: one core restricted-peer classifier remains authoritative; config/default/patch/camelCase wiring aligns; four magnet cache-miss add paths enter the one private collector; no raw peer-vector bypass or privacy-log leak remains; UI speaks only to explicit future BT connections and carries no policy logic.

### S-01 final slice conclusion

- The S-01 acceptance criteria now have strict-TDD, independent review, frontend quality and combined workspace test/build/clippy evidence.
- No source edit/commit/push has been performed by controller beyond the isolated worktree changes; user has not requested a commit.

### Open parent-task boundary

- The audit task itself is not complete: S-02 needs design repair/re-review/user authorization; S-03/S-05/P-01 and governance findings were classified, not implemented; S-04 has local acceptance plus combined workspace coverage but remains subject to its documented environment-specific default-feature limitation history.
- Decision: S-01 requirement-verified; parent audit task paused-for-user on next bounded priority.

## Checkpoint 16 — S-04 final review found test-hook isolation defect; strict remediation restarted

### Finding and root cause

- Final S-04 integration review requested changes: the `#[cfg(test)]` global one-shot PlanComplete hook is unconditionally consumed by every test-build `load_plan_snapshot` call. Other concurrently run PlanComplete tests can take a hook installed for a target test.
- Independent diagnosis confirmed this is real in same-binary Rust test execution; nextest process behavior cannot be the correctness mechanism. The existing install lock protects only hook-installing tests, not unrelated PlanComplete tests.

### Approved minimal remediation

- Canonical owner and only allowlisted file: `crates/tachyon-app/src/runtime/chunk_reader_pool.rs`.
- Tester must first add a deterministic nonmatching-task hook isolation regression and observe RED.
- Coder may then convert the test-only hook slot to task-ID-directed `take_for`, preserving one-time/RAII semantics. No production job injection, generic executor, new dependency, or test serialization policy is authorized.
- Existing PlanComplete timing/fallback contracts remain frozen.

### Scope status

- S-04 is reopened at slice level until remediation passes independent Tester and Reviewer gates.
- S-01 remains independently approved and combined-workspace verified; its result is not invalidated by this test-only S-04 defect.

### Drift check

- This is a test-isolation repair at the existing test seam, eliminating an accidental global consumer path rather than expanding production design.
- Decision: continue strict S-04 remediation; no other audit implementation starts.

## Checkpoint 17 — S-04 test-hook remediation accepted; combined verification refreshed

### Strict remediation closure

- Tester repaired an initial test-only compilation issue, then observed the intended behavior RED: a nonmatching `other` PlanComplete consumed the target hook and triggered the explicit isolation failure.
- Coder implemented the narrow task-ID-directed test seam: `InstalledPlanSnapshotHook { expected_task_id, hook }` plus `take_for`; nonmatching jobs retain the slot and use the true store path. Existing tests only received mechanical task-ID installation arguments.
- Independent Tester approved after the isolation test, barrier/JoinError tests, 17 chunk_reader_pool-related app tests, Rustfmt, diff check and tachyon-app clippy all passed.
- Independent Reviewer approved the remediation: test-only scope, RAII cleanup and target-task isolation are correct; production loader/API remain unexpanded.

### Refreshed combined evidence

- Controller ran fresh `cargo fmt --all -- --check` and `git diff --check`: passed.
- Controller ran fresh `cargo nextest run --all`: 1914 passed, 0 skipped, 0 failed.
- Controller ran the frontend production build: passed (Vite production bundle).

### Status

- S-04 is again locally accepted through strict TDD and independent review.
- S-01 remains accepted; its frontend build is now additionally freshly verified.
- Parent audit task is still open only as a decision/triage task: no S-02 implementation is permitted until its design blockers are corrected and separately authorized; S-03/S-05/P-01/governance claims remain classified for distinct future scope.

### Drift check

- Remediation reduced test-seam entropy by making the global hook’s intended ownership explicit; no runtime fallback, dependency or API was introduced.
- Decision: S-01 and S-04 requirement-verified locally; pause-for-user for next authorized scope.
