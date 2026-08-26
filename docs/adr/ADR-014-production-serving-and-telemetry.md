# ADR-014: Production Serving and Telemetry - Live Dashboard, Honest OTLP, Graceful Signals

## Status

Accepted

## Context

Every pipeline phase is implemented and verified (ADR-003 through
ADR-013), but three facts kept a production deployment dishonest:

* `telemetry.otlp_endpoint` was accepted by configuration and never
  read by any binary - a dead knob on the exact surface operators use
  to prove a deployment is healthy. Configuration that does nothing is
  simulation by another name.
* The dashboard existed only as a dev-server story (`pnpm dev`
  proxying to the API). Nothing served its built assets, so going live
  required an undocumented second process whose credentials-injection
  contract lived in someone's head.
* Both binaries treated SIGTERM as an unhandled kill: only Ctrl-C was
  graceful. Every mainstream process manager stops services with
  SIGTERM, so every real restart would have skipped connection drain
  and job-lease cleanup by construction.

The API surface itself was complete; what was missing was the
operational shell around it.

## Decision

### One telemetry path for both binaries

A new `hephaestus-telemetry` crate owns process observability setup.
It installs the structured log subscriber exactly as before and, when
`telemetry.otlp_endpoint` is set, builds a real OTLP/HTTP span
exporter (reqwest client, protobuf) behind a tracing layer tagged with
the configured `service_name`. Absent endpoint means local logging
only - the honest default for laptops. A returned guard shuts the
tracer provider down explicitly so in-flight spans flush before exit;
both the server and the worker call it on their way out. The endpoint
setting can no longer be silently ignored: it either wires export or
fails startup loudly (unreachable collectors are an operator problem,
not a configuration lie).

### SIGTERM drains like Ctrl-C

Both binaries resolve shutdown from either signal source through one
shared helper (Ctrl-C or SIGTERM on Unix). The worker keeps draining
in-flight jobs under held leases; the server stops accepting
connections and finishes open responses. Process managers now get the
same grace the interactive path always had.

### The server serves the dashboard it was built for

An optional `[web]` configuration section points the API binary at
the built dashboard (`dist_dir`) and optionally injects its runtime
configuration (`base_url`, `token`, `poll_seconds`). When
configured, hephaestus-server serves those static files on GET/HEAD:
hashed `/assets/*` responses are immutable-cacheable, `index.html`
is served with the placeholder bootstrap replaced once at startup by
the configured `window.__HEPHAESTUS_WEB_CONFIG__` object, unknown
non-API paths fall back to the SPA entry, and everything under
`/api` keeps the exact JSON error contract - the OpenAPI inventory is
untouched because serving files adds no operations. A dist bundle
without the expected placeholder fails startup loudly instead of
serving a dashboard that would silently ignore its configuration.
Same-origin defaults hold (`base_url = ""`), so one process fronts
both the control plane and its human surface. Not configuring `[web]`
preserves today's behavior byte for byte; external fronting stays
supported and documented.

### Bounded requests

`server.request_timeout_secs` (default 30, bounded 1..=600) applies a
request timeout to the whole router. Handlers that legitimately wait -
readiness pings, gate decisions - run far inside it; hung database
calls or stalled clients release their worker instead of pinning it.

## Consequences

* `HEPHAESTUS_OTLP_ENDPOINT` is a promise again: setting it produces
  exported spans, verifiable against any OTLP/HTTP collector.
* Restarting the control plane under launchd/systemd drains cleanly;
  leases expire honestly instead of workers dying mid-job.
* Production deployment becomes one binary plus PostgreSQL: server,
  API and dashboard from one origin, credentials injected at start.
* The web tier gains no authority - it still renders state and
  forwards commands; all enforcement remains server-side (ADR-001).
