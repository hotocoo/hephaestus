# ADR-013: Deployment Pipeline - Configured Targets, Operator Role, Post-Deploy Verification

## Status

Accepted

## Context

Delivery ends one hop short of done: after a merge decision the build
stage either completes a run whose plan leaves deployment unset
(`skip_deployment`) or fails it loudly when the plan demands one,
because no executor existed (ADR-008). The groundwork was laid
deliberately - `Building -> Deploying -> VerifyingDeployment ->
Completed` transitions exist, the `deployment` queue name is reserved
but refused by worker validation, and the typed `DeploymentOutcome`
event was declared up front. What was missing is the honest middle:
something that really deploys when configuration says so, and still
refuses to pretend when it does not.

## Decision

### Targets are named, configured commands

A deployment target lives in configuration (`[deployment.targets]`,
TOML only - nested command vectors have no honest env encoding) as an
argv vector plus mandatory post-deployment verification hooks:

```toml
[[deployment.targets]]
name = "staging"
command = ["heph-deploy", "--env", "staging"]
verify = [["heph-probe", "--ready"], ["heph-smoke"]]
timeout_secs = 600
```

The plan's strategy note names the target: `strategy.deployment`
trimmed must equal one configured target name exactly. There is no
prose parsing and no default target - a plan naming nothing matchable
fails the run at routing time with the valid names listed, before any
command runs. Commands execute through the governed sandboxed shell as
argv vectors inside the run workspace, like every other deterministic
stage; network egress stays denied everywhere, unchanged.

### The operator role extends, not breaks, ADR-005

A fourth built-in role, `Operator`, holds `WorkspaceRead`, `ShellExec`
and the previously forbidden-everywhere `Deploy` capability - but its
manifest exists only as derived from a concrete target's commands, and
it can never hold `SecretRead`, `NetworkEgress` or workspace write.
Separation of duties stays encoded in policy: operators deploy, they do
not mutate sources or touch secrets. The capability ceiling is checked
by the same manifest validation as every other role, so granting more
than the named target's commands fails closed.

### One job, two durable phases, replay-safe throughout

When the build succeeds and the plan demands deployment, the build
stage bootstraps the running `deployments` row (partial unique index:
one live row per run, mirroring builds), advances
`Building -> Deploying`, and chains one `deploy:{run}` job. The
handler drives both remaining phases as separate idempotent steps:

1. *Deploy* - run the target command under the operator manifest;
   success advances `Deploying -> VerifyingDeployment`.
2. *Verify* - run each verification hook under the same ceiling; every
   hook exiting zero finishes the row as verified and advances to
   `Completed`.

Every step re-derives its necessity from visible durable state, so a
crash between any two of them resumes instead of double-spending:
redelivered jobs skip finished phases, stale redelivery after a
terminal state is absorbed silently. Hooks are mandatory per target -
a deployment nobody can check is a simulation risk, not a feature.
Any failure finishes the row as failed, persists the reason, fails the
run terminally, and appends the typed `DeploymentOutcome` event in the
same transaction as the row's terminal status. Evidence and state
never diverge.

### Queue governance follows reality

Worker validation admits `deployment` exactly when targets are
configured and refuses it otherwise (the old loud error, kept precise).
The vocabulary pin between `WORKER_QUEUE_NAMES` and `engine::Queue`
now agrees trivially: every engine queue is servable, each only under
its own preconditions - model-backed queues demand provider settings,
deployment demands targets. The job payload schema bumps to v3 for the
new payload; older envelopes dead-letter loudly rather than being
guessed at, consistent with the versioned-payload contract.

### One read endpoint, mirrored everywhere

The dashboard needs to show what happened: GET
/api/v1/runs/{run_id}/deployment returns the run's latest deployment
row (404 when none), scoped by tenant, entering the same OPERATIONS
inventory, zod mirror and generated SDK types as every other
operation. Rollback stays where the state machine left it: a legal
transition no code exercises yet - claimed nowhere, implemented
nowhere.

## Consequences

* The happy path now reaches `completed` through a real deployment:
  merged, built, deployed, verified - each step evidenced.
* Runs demanding deployment without matching configuration still fail
  loudly; the no-simulation contract survives contact with features.
* Adding a target is configuration; adding a rollback executor or
  remote orchestration remains future work with its own ADR.
* Post-deployment verification reuses the hook pattern later phases
  can extend (per-environment probes, artifact attestations) without
  reshaping the table or the event.
