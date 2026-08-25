# ADR-008: Delivery Pipeline (Merge Gate and Builds)

## Status

Accepted

## Context

Execution ends at `awaiting_merge`: verification passed, the
read-only reviewer approved, and the change set sits in the run's
workspace. Someone must decide to merge, and something must turn a
merged change set into either a completed task or an honest failure.
The state machine already names the legal path - `Merged`,
`BuildSucceeded`, `SkipDeployment`, `DeploymentFinished`,
`DeploymentVerified` - but no stage drove it.

Two forces shape the design. First, merging is genuinely external:
it happens on the platform where the code lives, performed by humans
or CI, and Hephaestus must never invent it. Second, the repository's
contract forbids simulated integrations: until real deployment target
configuration exists, no stage may claim a deployment happened.

## Decision

### The merge gate is an approval gate

A merge is recorded as a decision on the run's open `merge` row in
the existing `approvals` table - the same mechanism, decision
semantics and `ApprovalDecided` event as every other human gate.
There is no "rejected" merge: rejecting a change set happens at the
review gate or by cancelling the task. The review stage opens the
gate atomically with parking the run at `awaiting_merge`, exactly as
planning opens its gate before `awaiting_approval`, so operators can
enumerate pending merges.

### Every step of applying a decision replays

Recording the decision, creating the build evidence row, advancing
`awaiting_merge -> building` and chaining the build job are separate
durable steps. Each is individually idempotent, and re-issuing the
same decision re-derives its outcome from visible state instead of
double-spending: a decided-but-unadvanced gate resumes mid-chain, a
run already in `building` re-chains the idempotency-keyed job, and a
terminal run absorbs the replay silently. This closes the residual
gap ADR-007 accepted for plan approvals.

### Builds are deterministic commands through governed access

The build stage pins one command (`cargo build --workspace`) and
executes it through the tool runtime under Verifier capabilities -
read plus allowlisted cargo only. The change set is frozen post-merge;
nothing model-driven touches it again. Like verification layers,
widening the command set is a reviewed code change, not configuration.

### Evidence, not logs

Each build gets its own row in the new `builds` table, anchored to
the execution whose change set was built. A partial unique index
admits one RUNNING build per run; success records a SHA-256 digest
over the executable artifacts (ordered by workspace-relative path) -
a stable fingerprint rather than a reproduction claim. The typed
`build_completed` event is appended in the same transaction as the
row's terminal status.

### No deployment executor means loud failure

When the plan's strategy demands deployment and none exists yet, the
stage fails the run terminally after recording that the build itself
succeeded. Both facts stay honest in evidence: the build row reads
"succeeded", the run reads "failed" with the reason persisted.
Faking completion would violate the no-simulation contract; silently
ignoring the strategy would drop a requirement the planner made.

## Consequences

* Runs whose plans leave deployment unset complete through
  `skip_deployment`; the happy path reaches `completed` end to end.
* Deployment lands later as a real integration: configured targets,
  a dedicated operator role extending ADR-005's forbidden-capability
  list, and post-deployment verification. Nothing in this phase needs
  rewriting when it does - the state machine hop already exists.
* Artifact storage stays where cargo put it; the recorded digest and
  path are enough for the future artifact system to locate and verify
  outputs without Hephaestus becoming a blob store today.
