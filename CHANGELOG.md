# Changelog

All notable changes to Hephaestus are documented here.
Format follows Keep a Changelog; versioning is Semantic Versioning
until the 1.0 API contract freezes per ADR-011.

## [Unreleased]

### Added

- Core domain model: typed UUIDv7 identifiers, structured error
  taxonomy with stable public codes, explicit workflow state machine
  (property-tested), versioned event envelopes with provenance.
- Prompt-injection defense primitives: untrusted-content framing,
  injection indicator scanning, secret redaction.
- Layered validated configuration (defaults -> TOML -> env) with
  fail-closed production checks.
- PostgreSQL persistence: initial schema migration, tenant-scoped
  stores, idempotent task/job creation, workflow runs with CAS
  transitions and atomic event writes, durable job queue with
  priorities/retries/dead-letter/scoped lease recovery, tamper-evident
  audit log with hash-chain verifier.
- Project governance docs: SECURITY.md, CONTRIBUTING.md, Code of
  Conduct, CI workflows, issue/PR templates.
- Repository intelligence: safe git operations via argument vectors,
  tracked-file inventory with language classification, deterministic
  AST-based symbol extraction behind per-language extractors.
- Centralized tool runtime: builtin tool registry, deny-by-default
  capability sets with workspace containment and command allowlists,
  sandboxed argv-vector shell execution, audited invocation entry.
- Layered deterministic verification engine: ordered verification
  plans executed through the governed tool runtime; no successful
  state without every required layer passing.
- Agent runtime: declarative role manifests with policy invariants
  (reviewer read-only, verifier cannot mutate sources, least
  privilege enforced at validation), OpenAI-compatible model provider
  with bounded retries, and a governed session loop where every tool
  call is authorized by the runtime, audited through pluggable sinks,
  budget-bounded, and re-framed as untrusted data before reaching the
  model again.
- Planning pipeline: repository snapshot via hardened git plus
  deterministic inventory analysis, requirement extraction and plan
  generation driven by governed planner sessions whose final answers
  are strict JSON documents parsed deterministically (malformed
  output retries within queue budgets, never guessed at);
  transactional requirements replacement and plan persistence with
  automatic supersession of prior plans; an approval gate that opens
  atomically with the generated plan and parks the workflow run at
  `awaiting_approval`. Handler-level failure classification maps
  transient problems to bounded retries and configuration or data
  problems to permanent failure.
- Execution pipeline (ADR-007): a decision service over the plan
  approval gate whose durable effects are individually idempotent and
  safely replayable after crashes; executions that snapshot approved
  plan steps into their own progress rows before work starts;
  governed Implementer sessions executing one step at a time against
  strict JSON outcome documents ("completed"/"blocked", summary,
  changed-file evidence); deterministic verification suites mapped
  from each plan's required layers and executed through the governed
  tool runtime with Verifier capabilities only, recorded as
  append-only evidence; fix and review loops that re-enter
  implementation through repair sessions framed by verification
  output or reviewer findings, with budgets owned by the execution
  layer as counts over durable evidence rows; blocked implementations
  fail the run loudly instead of guessing; a read-only Reviewer gate
  (hard policy invariant) that approves over the working-tree diff
  and parks finished work at `awaiting_merge` pending human or CI
  merge.