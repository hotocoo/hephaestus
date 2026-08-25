# ADR-010: Worker Runtime

## Status

Accepted

## Context

Every pipeline phase so far - intake, planning, execution, review,
delivery - is driven by durable jobs and exercised by integration
tests that assemble handler registries by hand. Nothing outside test
code processes those jobs. The API server deliberately holds no model
credentials and serves only HTTP (ADR-009); its decision records that
job processing "remains the worker's role". That role now needs a
real process: one that assembles the full registry from layered
configuration, holds the model boundary where it belongs, and runs
until operators stop it.

## Decision

### One new crate: hephaestus-worker

The binary loads configuration, connects to PostgreSQL, assembles the
`HandlerRegistry` from configuration, and runs the durable worker
loop until SIGINT. In-flight jobs drain gracefully under their leases;
no new claims start after shutdown begins.

The worker never applies migrations. The API server owns schema
evolution (ADR-009); starting a worker against an unmigrated database
fails loudly on first claim instead of racing another process through
the versioned migration path.

### Model credentials live here and only here

Only the worker builds model providers. It constructs the
OpenAI-compatible provider exactly when its configured queues include
a model-backed stage (planning, implementation, review); a worker
serving deterministic queues alone requires no provider configuration
and builds none. Validation enforces both directions in
`hephaestus-config`: model-backed queues demand `base_url` and
`model` at load time (plus `api_key` in production), while the
deterministic defaults validate cleanly with zero credentials present.
A misconfigured worker therefore dies at startup rather than retrying
every job into dead-letter.

### Queue surface is explicit and honest

Operator-selected queue sets are validated at load: unknown names are
rejected with the valid set listed, duplicates rejected, empty sets
rejected, and `deployment` refused outright. No deployment executor
exists (ADR-008) - runs whose plans demand deployment fail loudly by
design - so configuring worker capacity for a queue that can never
legitimately receive jobs would be simulation by another name. The
default configuration serves only deterministic queues (analysis,
verification, build). The config vocabulary is pinned against
`engine::Queue` by a test in the worker crate so the two lists
cannot drift; the engine exposes `Queue::iter_all` for exactly that.

### Governance unchanged

Model-backed stages run through the same `SessionDeps` plumbing the
tests exercise, with the production `AuditLogSink` writing every
authorization decision into the hash-chained audit log. Handlers keep
their role manifests' capability ceilings; the binary adds
orchestration capacity, not authority. Nothing in the registry can
grant what the manifests forbid.

### Single-node story

One server plus one or more workers against one database is the
supported shape today. Process supervision (systemd units, containers)
is an operations concern outside this repository's scope; nothing here
fakes it.

## Consequences

* The platform runs end-to-end outside tests for the first time:
  submit through the API, workers drive pipelines to
  `awaiting_approval`, `awaiting_merge`, or `completed`.
* Scaling is per-queue: operators size worker pools to queues
  explicitly instead of one monolith absorbing everything.
* Key rotation remains config-change-plus-restart, consistent with
  the identity phase deferral recorded in ADR-009.
* When a real deployment executor lands, it extends this registry -
  the `deployment` queue name already exists and stays unclaimable
  until then.
