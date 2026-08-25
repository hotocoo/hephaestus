# ADR-009: HTTP API Layer

## Status

Accepted

## Context

Every pipeline phase so far - intake, planning, execution, review,
delivery - is driven by durable jobs and human gate decisions recorded
through library calls. Nothing outside the process can submit a task,
observe a run, or act on a gate. ADR-001 reserves that door for one
component: an axum API layer the web tier talks to. It must expose real
state honestly, never widen permissions, and keep tenant scoping
enforceable at every endpoint.

## Decision

### One new crate: hephaestus-api

The crate contains the router, handlers, request/response types, error
mapping, authentication middleware, and the small `hephaestus-server`
binary that loads configuration, applies migrations, binds the socket,
and serves. Handlers call existing services (intake, approval, merge)
and store reads; they contain no business rules of their own.

### Versioned surface, tenant-scoped by construction

All endpoints live under `/api/v1` plus unauthenticated liveness and
readiness probes (`/healthz`, `/readyz`). The authenticated principal
carries its organization; handlers resolve entities only through
org-scoped store methods, so a wrong-tenant identifier is
indistinguishable from a missing one (404). There are no org ids in
request bodies to forge.

### Authentication is pre-provisioned bearer keys

There is no token issuance, no password flow, no identity store yet.
Configuration holds API keys (`[auth.keys]` or `HEPHAESTUS_AUTH_KEYS`),
each binding a bearer token to exactly one organization and one
principal. Tokens are validated in constant time where comparison
dominates, redacted from all Debug output, and required non-empty in
production when auth is enabled (fail closed). Disabling auth is legal
only in development and test, as before; disabled mode still requires
an explicit organization header because tenant scoping is never
inferred. Issuance and rotation tooling lands with the identity phase.

### Errors map onto the core taxonomy

Handlers return `hephaestus_core::Error`; one mapper turns it into
HTTP: validation 422, not-found 404, conflict 409 (including illegal
workflow transitions), unauthenticated 401, forbidden 403, budget
exhausted 429, storage and external faults 500 with details logged and
never serialized. Response bodies carry the stable public code from
the taxonomy plus safe messages only.

### Gate decisions go through the services, not around them

Plan approvals and merge decisions are recorded by calling
ApprovalService and MergeService - their idempotent, crash-replayable
paths - never by writing gate rows directly. ApprovalService gains a
decision-only constructor (`gate_only`) because the decision path uses
durable storage alone; the API binary deliberately holds no model
provider credentials, and workers continue to run sessions.

### Read surfaces stay bounded

Task, run, plan, approval-gate, execution-step, build, event, project
and repository listings are paginated or hard-capped, org-scoped, and
rendered from typed rows. Events render provenance-tagged payloads as
data. Nothing here can trigger a transition except through the two
gate-decision endpoints and task intake.

### Deployment story unchanged

The server performs migrations at startup (the versioned path) and
serves HTTP. Job processing remains the worker's role; single-node
supervision of both processes is a later operations decision, not
something this layer fakes.

## Consequences

* The platform is drivable end-to-end from outside the process for the
  first time: submit work, watch state and evidence, act on gates.
* The web tier (ADR-002) can be built against a stable contract;
  conformance tests will pin it.
* Key rotation today means config change + restart; acceptable until
  the identity phase, and honest about it.
* The server binary runs migrations; operators must not point many
  servers at one database before the migration lock story is revisited.
