# ADR-007: Execution Pipeline

## Status
Accepted

## Context
Planning ends at `awaiting_approval`. Someone must turn an approved
plan into working software: execute its steps, verify the result
deterministically, and route failures somewhere bounded. The hard
parts are not the happy path but the loops - verification fails,
reviews request changes, implementers get stuck - and recovery,
because every stage can crash between any two durable effects.

## Decision

### One active execution per run
Approval creates an `executions` row over the current plan. A partial
unique index (`WHERE status = 'active'`) makes "one live attempt per
run" a storage guarantee, not a convention. On creation, plan steps
are snapshotted into `execution_steps`: later plan supersession can
never rewrite what an execution is committed to do.

### Steps run through governed sessions, one at a time
Each step is a governed Implementer session (fs read/write plus a
cargo/rustfmt shell allowlist) whose final answer MUST be one JSON
document: {"status": "completed|blocked", "summary": "...",
"files_changed": [...]}. Requirements, the plan objective and prior
step summaries travel as framed untrusted facts. Malformed documents
retry within queue budgets; a blocked implementation is terminal -
the run fails loudly with the reason persisted rather than guessing
or stalling silently.

### Verification layers are code, not configuration
Plans name canonical layers ("format", "lint", "typecheck",
"unit_tests"); this module pins each to a concrete cargo command and
executes it through the tool runtime under Verifier capabilities
(read + allowlisted cargo only). Unknown or duplicated layer names
fail closed: verification never silently drops a layer the planner
demanded. An empty strategy falls back to a deterministic default
set - the same philosophy as role-manifest defaults: operators may
narrow per task, nothing widens by configuration. Every suite run is
appended to `verifications` with its serialized report.

### Loop budgets are counts of evidence rows
The fix loop re-enters implementation via repair sessions framed
with failing-layer output; the review loop does the same with reviewer
findings. Budgets are NOT separate counters that can drift: they are
`COUNT(*)` over failed verification rows and request-changes verdict
events respectively. When `MAX_FIX_ROUNDS` (3) or `MAX_REVIEW_ROUNDS`
(2) is exceeded, the execution closes as failed and the workflow run
takes the legal terminal Fail transition.

### Review is read-only and diff-driven
The Reviewer role holds fs.read only by hard invariant (ADR-005). It
receives the working-tree diff against HEAD as untrusted repository
data (untracked scratch files are invisible by design) plus the plan
objective, and must answer with exactly one verdict document.
Approval closes the execution as passed and parks the run at
`awaiting_merge`; an empty diff requests changes deterministically
without spending a model call.

### Recovery contract
Decision recording, transition, execution bootstrap and job chaining
are separate durable steps - deliberately not one transaction, since
they span subsystems. Instead, every step is idempotent (gate
consumption, partial unique index, idempotency-keyed enqueues) and
the service derives replayed outcomes from already-applied state:
re-issuing a decision whose effects are visible returns the same
outcome instead of double-spending or dead-ending. The residual gap -
a crash after gate consumption but before any visible effect -
requires operator attention and is logged loudly; folding decision
and bootstrap into one outbox-style transaction belongs to the API
phase.

## Consequences
* Merging is genuinely external: runs park at `awaiting_merge` until
  humans or CI act; later build/deploy phases chain from that event
  rather than inventing automatic merges.
* Verification commands evolve only through reviewed code changes to
  this mapping, keeping the "no successful state without every
  required layer passing" spec auditable.
* Evidence for every loop iteration (verification reports, review
  verdicts) is durable and tenant-scoped, ready for the future
  evidence graph.
* Job payloads moved to schema version 2 (`repair_execution`,
  `run_review` added); v1 envelopes are rejected loudly during rolling
  upgrades.
