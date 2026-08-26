# ADR-012: Web Dashboard - Vue 3 Shell Over a Verified Client

## Status

Accepted

## Context

ADR-001 promised a Vue 3 tier; ADR-011 delivered its foundation -
a served OpenAPI document, strict zod mirrors, and a typed SDK whose
declarations cannot drift. The control plane was runnable end to end
but observable only through curl. What remained was the human surface:
a dashboard that renders server state and forwards human decisions
without becoming an authority of its own.

One wire-contract gap blocked it: a task page needs its run's live
state, but no endpoint mapped a task to its run without knowing the
run id from an intake receipt.

## Decision

### One small API addition, mirrored everywhere

The API grows exactly one operation: GET /api/v1/tasks/{task_id}/run,
returning the task's current run (newest by UUIDv7 ordering), scoped
by organization, rendering 404 when no run exists yet. It enters the
same three mirrors as every other operation - OPERATIONS inventory,
zod conformance, regenerated SDK types - so drift still fails CI from
all directions.

### The shell owns wiring; views own nothing

The Vue app mounts only when runtime configuration supplies a bearer
token (window.__HEPHAESTUS_WEB_CONFIG__ in index.html). Without
credentials it renders a setup screen and fires no requests - the API
defines no interactive login, so the UI invents none. With credentials,
the real HephaestusClient is provided once; the shared catalog
(projects, repositories) loads once beside it. Every view pulls server
data through one composable (useAsyncResource) that distinguishes
loading, failed-with-public-code, and empty - and never fabricates
content for the first two.

### Views

* **Overview** - catalog counts, recent tasks with their current run
  state (fetched for the visible page only; the overview never fans
  out over the whole task list), and the newest work sorted by age.
* **Tasks** - the full list ordered priority-then-recency plus the
  intake form, which mirrors CreateTaskRequest field-for-field and
  generates a fresh idempotency key per submission attempt (regenerated
  after any failure so edited content can never collide).
* **Task detail** - fields, plan steps with their verification hooks,
  workflow timeline, both human gates with their decision forms, and
  the event history.
* **Run detail** - lifecycle spine position, gates, events; reached
  directly or from a task page.

### State rendered as data; decisions forwarded as data

Events render provenance badges always, payloads as bounded monospace
text - never markup, never instructions. Well-known transition shapes
may enrich a headline but anything else falls back to raw text without
failing. Gate panels collect exactly the fields the wire contract
accepts; the approver stays the authenticated principal because the
server says so, not because the form omits an input.

Live views poll on a configured interval through one composable whose
timers die with the component scope. Unknown future states (a newer
server talking to an older UI) render verbatim at full progress rather
than being guessed into the spine.

### Tests mirror the discipline

Component tests mount the real App against an in-memory ControlPlane
double - the same structural interface the generated client satisfies
- and drive real flows: approval posts through the client, the gate
flips, the view reloads. The live suite spawns the real server binary
on real PostgreSQL and drives intake idempotency, the new task->run
lookup, and a mounted shell rendering actual HTTP responses.

## Consequences

* The platform is now human-operable end to end: submit, watch, approve,
  merge - all through the same authenticated surface the workers serve.
* The web tier stays authority-free by construction: credentials are
  deployment configuration, decisions carry no attribution, and no
  view computes policy.
* New endpoints cost one documented operation plus two automatic
  mirrors; the dashboard consumes them without hand-written plumbing.
* Polling is honest about being polling; when the control plane grows
  subscriptions, the composables change, the views do not.
