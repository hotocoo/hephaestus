# ADR-015: Artifact Registry and Serving

## Status
Accepted

## Context
ADR-008 left the build stage recording a single aggregate SHA-256 over
the collected artifact directory on the `builds` row - "enough for the
future artifact system to locate and verify". Individual artifacts
were therefore evidence-shaped but not locatable: nothing answered
"which files did this build produce, how big are they, what are their
hashes", nothing served them to an authenticated operator, and no
surface could re-verify that the bytes on disk still match what the
build recorded.

Artifacts live in the run workspace under the configured storage root
(`<storage.root>/runs/<run_id>/repo/target/debug`), produced by the
pinned build command (ADR-008) inside the governed sandboxed shell.
The worker that builds owns that disk; the API server so far did not
touch it.

## Decision
Hephaestus gains an artifact registry, served through the API:

1. **Per-file registry (migration 0005).** When a build succeeds, the
   same single pass that computes the aggregate hash now also records
   one row per executable file - workspace-relative path, content
   SHA-256, size in bytes - in an `artifacts` table keyed by build and
   path. Rows land in the SAME transaction that closes the build row,
   so state and evidence never diverge (the ADR-007/008 rule).
   Rebuilding a run appends a new evidence set; nothing is mutated or
   deleted.
2. **Paths are contained.** Registry paths are workspace-relative,
   validated at insert (absolute paths and `..` segments are refused
   by both Rust validation and a CHECK constraint) and resolved at
   read time only under the run's workspace directory, reusing the
   containment discipline of the tool runtime.
3. **Serving.** The API grows three tenant-scoped operations:
   `GET /api/v1/runs/{run_id}/artifacts` (registry listing),
   `GET /api/v1/runs/{run_id}/artifacts/{artifact_id}` (the bytes,
   streamed, with the expected SHA-256 in `ETag` and a response
   header so clients can verify independently), and
   `GET /api/v1/runs/{run_id}/artifacts/{artifact_id}/verification`
   (server-side re-hash: `verified`, `missing` or `corrupt` with the
   expected and actual digests). Downloads are efficient streams, not
   buffered verification passes; verification is explicit and separate
   - deterministic systems verify, and this one says what it found.
4. **Loud failures.** A registry row whose file vanished from disk
   downloads as 404 ("artifact file not found"), never as empty
   bytes; a file whose bytes no longer hash to its recorded digest
   verifies as `corrupt` and downloads are still byte-faithful (the
   mismatch is data about the disk, not a license to withhold or
   fabricate content). The API process resolves the storage root from
   the same layered configuration as the worker; without a configured
   root the artifact endpoints fail loudly instead of pretending.
5. **Single-node disk.** This ADR keeps artifacts on one node's disk,
   matching ADR-001's single-node mode. Multi-node artifact storage
   (object store, worker/API split) is future work with its own ADR.

The dashboard gains an Artifacts panel on the run view: every recorded
file with its size and short digest, independent verification per
file, and downloads fetched through the typed SDK (bearer
authenticated, saved via a blob URL - tokens never ride URLs).

## Consequences
* Build evidence becomes individually locatable and independently
  re-verifiable at any time after the build, closing ADR-008's
  deferred promise.
* The API process now reads the storage root; deployments where the
  worker's disk is not the API's disk must wait for the multi-node
  artifact-store ADR rather than half-work.
* `finish_build` gains an artifact-carrying form; the old signature
  remains for failed builds (which record no artifacts).
* Job payload schema stays at v3: the artifact rows are written by the
  same build job, not a new one.
* The wire surface grows by three operations mirrored through the
  OpenAPI inventory, zod contracts and generated SDK types like every
  other (ADR-011).
