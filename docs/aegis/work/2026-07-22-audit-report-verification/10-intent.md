# Task intent — 2026-07-22 audit report verification

## Requested outcome

Independently validate the technical claims in the local, untracked audit report
`D:/Rust/Tachyon/Document/Tachyon系统性深度审计报告-2026-07-22.md` against the
frozen source baseline `58bc93923fe93031330589c96afc22ac55c23a6c`; if a report
finding is confirmed, repair or optimize only the findings whose source boundary,
acceptance behavior, and verification path are established.

## Scope

- Static source, configuration, CI, and test evidence at `58bc939`.
- Reproducible local validation commands.
- Strict TDD for each approved source repair: independent tester owns RED and
  regression tests; coder owns production code; an independent reviewer approves.
- Cross-agent review of claim classification and each implementation slice.

## Non-goals

- Do not alter the user’s dirty main worktree.
- Do not represent static evidence as a production/network/kernel benchmark.
- Do not implement externally constrained or underspecified roadmap items merely
  because the report lists them.
- Do not publish, push, or commit without a later explicit user request.

## Baseline read set

- `AGENTS.md` — architecture, test, security, and verification constraints.
- `.claude/rules/multi-agent-engineering.md` — mandatory Tester → Coder → Tester → Reviewer separation.
- `docs/architecture.md` — current architectural authority where a finding touches an owner boundary.
- `Document/Tachyon系统性深度审计报告-2026-07-22.md` — requested claim source (external/untracked, read-only source document).
- `58bc939` — immutable source baseline.

## Baseline usage draft

| Field | Value |
|---|---|
| Required refs | Above read set |
| Acknowledged before plan | AGENTS.md; multi-agent rule; report; frozen commit |
| Cited in plan | Report finding IDs and source paths only after independent confirmation |
| Missing refs | Architecture source must be read for any cross-module repair |
| Decision | verification in progress |

## Impact statement

The report mixes confirmed source facts, conditional platform risks, subjective
scoring, historical claims, and roadmap proposals. The task must not collapse
those categories. A repair may only target a presently reproducible source
behavior at its canonical owner and must preserve documented protocol and
persistence compatibility.

## TDD route

- Mode: auto
- Decision: strict
- Reason: user explicitly requested TDD; candidate security/correctness fixes are shared/core behavior.
- Verification: isolated RED command, GREEN command, affected-crate tests, formatting, clippy, and independent review.

## Stop conditions

Stop a proposed repair and report instead when the claim is false/stale,
requires an external dependency/API decision, lacks a stable acceptance contract,
or conflicts with the frozen baseline/architectural owner.
