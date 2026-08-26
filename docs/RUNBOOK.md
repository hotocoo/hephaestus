# Production Runbook

How to take the Hephaestus control plane from source to a live,
verified deployment (ADR-014). Every step is honest and checked:
anything that cannot be verified is treated as not deployed.

## Prerequisites

- Rust 1.90+, Node 22+, pnpm 11+ (build machine)
- PostgreSQL 16+ reachable from both server and worker
- One pre-provisioned bearer key per tenant (`principal,organization_id,token`;
  tokens are 16+ characters, generated out-of-band - there is no
  issuance endpoint by design)

## 1. Provision the database

```bash
createdb hephaestus_prod
# The server applies versioned migrations on start; workers refuse
# to run against an unmigrated database instead of racing it.
```

## 2. Build the artifacts

```bash
cargo build --release -p hephaestus-api --bin hephaestus-server \
                       -p hephaestus-worker --bin hephaestus-worker
pnpm install --frozen-lockfile && pnpm --filter @hephaestus/web build
# dist output: apps/web/dist
```

## 3. Configuration file (`/etc/hephaestus/hephaestus.toml`)

```toml
[server]
bind_addr = "0.0.0.0"          # behind your TLS terminator
port = 7300
request_timeout_secs = 30      # ADR-014: hung handlers release workers

[database]
url = "postgres://hephaestus:SECRET@db.internal:5432/hephaestus_prod"

[storage]
root = "/var/lib/hephaestus/data"

[auth]
[[auth.keys]]
token = "GENERATE-16+-CHARS"   # or HEPHAESTUS_AUTH_KEYS env
organization_id = "UUID-of-org"
principal = "dashboard"

[web]                           # ADR-014: serve the dashboard
dist_dir = "/srv/hephaestus/web"

[web.runtime_config]
base_url = ""                   # same-origin default
token = "SAME-OR-OTHER-KEY"     # injected into index.html at boot
poll_seconds = 10

[telemetry]                     # optional OTLP/HTTP export (ADR-014)
log_level = "info"
otlp_endpoint = "http://otel-collector.internal:4318"
service_name = "hephaestus-prod"

[worker]                        # worker process only
queues = ["analysis", "verification", "build"]

[[deployment.targets]]          # required only for the deployment queue
name = "staging"
command = ["heph-deploy", "--env", "staging"]
verify = [["heph-probe", "--ready"]]
timeout_secs = 600
```

Model-backed queues (`planning`, `implementation`, `review`) additionally
require `[model]` provider settings in the WORKER's configuration;
production validation fails closed when they are served without a key.

## 4. Run under a process manager

Both binaries drain on SIGTERM identically to Ctrl-C (ADR-014), so
restarts are safe mid-flight.

**systemd** (Linux):

```ini
# /etc/systemd/system/hephaestus-server.service
[Unit]
Description=Hephaestus control plane
After=network.target postgresql.service

[Service]
ExecStart=/usr/local/bin/hephaestus-server --config /etc/hephaestus/hephaestus.toml
Environment=HEPHAESTUS_ENVIRONMENT=production
Restart=on-failure
KillSignal=SIGTERM             # graceful: drains connections
TimeoutStopSec=60

[Install]
WantedBy=multi-user.target
```

```ini
# /etc/systemd/system/hephaestus-worker.service
[Unit]
Description=Hephaestus worker
After=hephaestus-server.service

[Service]
ExecStart=/usr/local/bin/hephaestus-worker --config /etc/hephaestus/hephaestus.toml
Environment=HEPHAESTUS_ENVIRONMENT=production
Restart=on-failure
KillSignal=SIGTERM             # graceful: drains jobs under held leases
TimeoutStopSec=300

[Install]
WantedBy=multi-user.target
```

**launchd** (macOS): same shape - `KeepAlive`, `EnvironmentVariables`,
SIGTERM on stop is launchd's default.

## 5. Verify the deployment

```bash
curl -fsS http://host:7300/api/v1/healthz      # {"status":"ok"}
curl -fsS http://host:7300/api/v1/readyz       # {"status":"ready"} = DB pinged
curl -fsS http://host:7300/api/v1/openapi.json | head -c 200
curl -fsSI http://host:7300/                   # text/html, cache-control: no-cache
```

The dashboard must render data, not its setup screen - if it renders
setup, the injected runtime config did not reach it; fix the config,
do not work around the screen.

## 6. End-to-end smoke

```bash
curl -fsS -X POST http://host:7300/api/v1/tasks \
  -H "authorization: Bearer $KEY" -H 'content-type: application/json' \
  -H 'idempotency-key: smoke-001' \
  -d '{"title":"Smoke","repository_url":"https://example.invalid/r.git", ...}'
```

Watch the run advance analysis -> planning (model-backed queues need
provider settings) -> `awaiting_approval`; decide through
`POST /api/v1/runs/{id}/approval`, then merge through the merge gate.
Deterministic stages proceed without any model involvement.

## Telemetry

Setting `telemetry.otlp_endpoint` exports spans over OTLP/HTTP
(protobuf) tagged with `service_name`; verified against any collector.
Absent endpoint means local structured logging only. Exporter faults
surface as tracing events - they are never swallowed silently.

## Rollback posture

Rollback of runs remains a legal state-machine transition no code
exercises yet (ADR-013); do not improvise one operationally. Failed
deployments fail their runs terminally with persisted reasons.